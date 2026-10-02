#!/usr/bin/env python3
"""Export nvidia/parakeet-tdt-0.6b-v3 to ONNX for sherpa-onnx, in fp32 and int8.

Adapted from k2-fsa/sherpa-onnx scripts/nemo/parakeet-tdt-0.6b-v3/export_onnx.py
(Apache-2.0, Xiaomi Corp., Fangjun Kuang). Differences:

* the relative-position table of the encoder is sized by --max-frames (encoder
  frames at 80 ms; NeMo's default pos_emb_max_len is 5000, about 400 s), via
  ConformerEncoder.set_max_audio_length() before tracing;
* the fp32 encoder (about 2.4 GB, over protobuf's 2 GB limit) is kept, saved with
  all weights in one external file `encoder.weights`;
* int8 variants: `int8` = the recipe's quantize_dynamic (QUInt8 encoder weights,
  QInt8 decoder and joiner), `int8-pc` = the same with per-channel weight scales,
  `int8-noattn` = per-tensor but the self-attention MatMuls stay fp32.

Model licence: CC-BY-4.0 (NVIDIA). Output layout per variant directory:
encoder[.int8].onnx (+ encoder.weights for fp32), decoder[.int8].onnx,
joiner[.int8].onnx, tokens.txt, bpe.vocab.
"""
import argparse
import os
import shutil
import sys
import time
from pathlib import Path

import onnx
import torch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "recipe"))

META_KEYS = None  # filled in main()


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def set_meta(model: onnx.ModelProto, meta: dict):
    while len(model.metadata_props):
        model.metadata_props.pop()
    for k, v in meta.items():
        p = model.metadata_props.add()
        p.key = k
        p.value = str(v)


def save_small(model: onnx.ModelProto, path: Path, meta: dict):
    set_meta(model, meta)
    onnx.save(model, str(path))


def save_external(model: onnx.ModelProto, path: Path, meta: dict, weights_name: str):
    set_meta(model, meta)
    onnx.save(
        model,
        str(path),
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location=weights_name,
        size_threshold=0,
        convert_attribute=False,
    )


def quantize(src: Path, dst: Path, weight_type, per_channel=False, nodes_to_exclude=None, meta=None):
    from onnxruntime.quantization import quantize_dynamic

    t = time.time()
    quantize_dynamic(
        model_input=str(src),
        model_output=str(dst),
        weight_type=weight_type,
        per_channel=per_channel,
        nodes_to_exclude=nodes_to_exclude or [],
    )
    if meta is not None:
        m = onnx.load(str(dst), load_external_data=False)
        set_meta(m, meta)
        onnx.save(m, str(dst))
    log(f"quantized {src.name} -> {dst} ({dst.stat().st_size/1e6:.0f} MB) in {time.time()-t:.0f}s")


@torch.no_grad()
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--nemo", default="parakeet-tdt-0.6b-v3.nemo")
    ap.add_argument("--out", default="out", help="output root; one sub-directory per variant")
    ap.add_argument("--max-frames", type=int, default=5000, help="encoder frames the position table covers (80 ms each)")
    ap.add_argument("--variants", default="fp32,int8,int8-pc,int8-noattn")
    ap.add_argument("--skip-trace", action="store_true", help="reuse <out>/raw/*.onnx from a previous run")
    args = ap.parse_args()

    import nemo.collections.asr as nemo_asr

    out = Path(args.out)
    raw = out / "raw"
    raw.mkdir(parents=True, exist_ok=True)
    variants = args.variants.split(",")

    t0 = time.time()
    if Path(args.nemo).is_file():
        asr_model = nemo_asr.models.ASRModel.restore_from(restore_path=args.nemo, map_location="cpu")
    else:
        asr_model = nemo_asr.models.ASRModel.from_pretrained(model_name="nvidia/parakeet-tdt-0.6b-v3", map_location="cpu")
    asr_model.eval()
    log(f"model loaded in {time.time()-t0:.0f}s; pos_emb_max_len={asr_model.encoder.pos_emb_max_len} pe rows={asr_model.encoder.pos_enc.pe.shape[1]}")

    # Position table. NeMo sizes pe to 2*max_len-1 rows at construction (max_len =
    # pos_emb_max_len = 5000 for this checkpoint). The forward slices
    # pe[:, center-T : center+T-1], which silently wraps for T > max_len; the stock
    # export therefore aborts for audio longer than 5000 frames (400 s).
    asr_model.encoder.set_max_audio_length(args.max_frames)
    log(f"position table set to {args.max_frames} frames: pe rows={asr_model.encoder.pos_enc.pe.shape[1]}")

    with open(raw / "tokens.txt", "w", encoding="utf-8") as f:
        for i, s in enumerate(asr_model.joint.vocabulary):
            f.write(f"{s} {i}\n")
        f.write(f"<blk> {i+1}\n")
    try:
        from generate_bpe_vocab import generate_bpe_vocab_from_model

        generate_bpe_vocab_from_model(asr_model=asr_model, output_path=str(raw / "bpe.vocab"))
    except Exception as e:  # hotword support only; not needed for the spike
        log(f"bpe.vocab skipped: {e!r}")

    normalize_type = asr_model.cfg.preprocessor.normalize
    if normalize_type == "NA":
        normalize_type = ""
    meta = {
        "vocab_size": asr_model.decoder.vocab_size,
        "normalize_type": normalize_type,
        "pred_rnn_layers": asr_model.decoder.pred_rnn_layers,
        "pred_hidden": asr_model.decoder.pred_hidden,
        "subsampling_factor": 8,
        "model_type": "EncDecRNNTBPEModel",
        "version": "2",
        "model_author": "NeMo",
        "url": "https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3",
        "comment": "Only the transducer branch is exported",
        "feat_dim": 128,
        "max_frames": args.max_frames,
        "exported_by": "steno spike E (own export)",
    }
    log(f"meta {meta}")

    if not args.skip_trace:
        # torch.onnx.export (legacy tracer) writes tensors over 2 GB total as one
        # external file per initializer next to the model; we re-pack below.
        cwd = os.getcwd()
        os.chdir(raw)
        try:
            t = time.time()
            asr_model.encoder.export("encoder.onnx")
            log(f"encoder traced in {time.time()-t:.0f}s")
            t = time.time()
            asr_model.decoder.export("decoder.onnx")
            asr_model.joint.export("joiner.onnx")
            log(f"decoder+joiner traced in {time.time()-t:.0f}s")
        finally:
            os.chdir(cwd)
    del asr_model

    enc = onnx.load(str(raw / "encoder.onnx"))  # loads external data into memory (2.4 GB)
    dec = onnx.load(str(raw / "decoder.onnx"))
    joi = onnx.load(str(raw / "joiner.onnx"))
    log(f"raw ir_version enc/dec/joi = {enc.ir_version}/{dec.ir_version}/{joi.ir_version}; opsets {[o.version for o in enc.opset_import]}")

    def variant_dir(name):
        d = out / name
        d.mkdir(parents=True, exist_ok=True)
        shutil.copy(raw / "tokens.txt", d / "tokens.txt")
        if (raw / "bpe.vocab").exists():
            shutil.copy(raw / "bpe.vocab", d / "bpe.vocab")
        return d

    # fp32: encoder with external weights (one file), decoder and joiner inline.
    fp32 = variant_dir("fp32")
    t = time.time()
    save_external(enc, fp32 / "encoder.onnx", meta, "encoder.weights")
    save_small(dec, fp32 / "decoder.onnx", meta)
    save_small(joi, fp32 / "joiner.onnx", meta)
    log(f"fp32 saved in {time.time()-t:.0f}s: " + ", ".join(f"{p.name} {p.stat().st_size/1e6:.0f} MB" for p in sorted(fp32.iterdir())))
    del enc

    from onnxruntime.quantization import QuantType

    attn_nodes = None
    if "int8-noattn" in variants:
        m = onnx.load(str(fp32 / "encoder.onnx"), load_external_data=False)
        attn_nodes = [n.name for n in m.graph.node if n.op_type == "MatMul" and "self_attn" in n.name]
        log(f"int8-noattn: {len(attn_nodes)} self-attention MatMul nodes stay fp32 (of {sum(1 for n in m.graph.node if n.op_type == 'MatMul')} MatMuls)")
        del m

    for name in variants:
        if name == "fp32":
            continue
        d = variant_dir(name)
        per_channel = name == "int8-pc"
        excl = attn_nodes if name == "int8-noattn" else None
        quantize(fp32 / "encoder.onnx", d / "encoder.int8.onnx", QuantType.QUInt8, per_channel=per_channel, nodes_to_exclude=excl, meta=meta)
        quantize(fp32 / "decoder.onnx", d / "decoder.int8.onnx", QuantType.QInt8, per_channel=per_channel, meta=meta)
        quantize(fp32 / "joiner.onnx", d / "joiner.int8.onnx", QuantType.QInt8, per_channel=per_channel, meta=meta)
        log(f"{name}: " + ", ".join(f"{p.name} {p.stat().st_size/1e6:.0f} MB" for p in sorted(d.iterdir())))

    log(f"all done in {time.time()-t0:.0f}s")


if __name__ == "__main__":
    main()
