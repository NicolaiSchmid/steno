#!/bin/sh
# Spike F on Forge: WER against FLEURS de references for the CoreML engines (steno dev
# bakeoff), the own fp32 ONNX export through the spike-D harness (sherpa-onnx recognizer,
# VAD chunker 25 s, 4 threads), and the glue-free numpy TDT loop on onnxruntime.
# Run under nohup from ~/steno-spikes: sh fleurs/run-forge.sh [cat|utt|ort ...] > fleurs/run.log 2>&1 &
# DYLD_LIBRARY_PATH must sit in the final command because SIP strips it from /bin/sh.
set -u
F=~/steno-spikes/fleurs
STENO=~/steno-calibration/repo/.build/release/steno
CH=~/steno-spikes/chunker
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
OWN=~/steno-spikes/models-own/fp32
PY=~/steno-spikes/fleurs-venv/bin/python
bakeoff() { echo "=== bakeoff $1"; uptime; $STENO dev bakeoff $F/$1 --engines parakeet-v3 whisperkit-large-v3-turbo --out $F/$1/bakeoff > $F/$1/bakeoff.log 2>&1; echo "exit $?"; uptime; }
harness() { echo "=== harness fp32 $1"; uptime; (cd $CH && DYLD_LIBRARY_PATH=$L ./target/release/onnx-speech-spike --models models --corpus $F/$1 --out $F/$1/harness --skip-diar --threads 4 --chunker vad --target-seconds 25 --slid-dir models/sherpa-onnx-whisper-tiny --vote --tag f-fp32 --asr-dir $OWN --asr-files fp32 > $F/$1/harness.log 2>&1); echo "exit $?"; uptime; }
ortdec() { echo "=== ort fp32 $1"; uptime; $PY $F/ort_decode_dir.py --dir $OWN --files fp32 --corpus $F/$1 --out $F/$1/ort --tag ort-fp32 --threads 4 > $F/$1/ort.log 2>&1; echo "exit $?"; uptime; }
for step in ${*:-cat utt ort}; do
  case $step in
    cat) bakeoff cat; harness cat ;;
    utt) bakeoff utt; harness utt ;;
    ort) ortdec cat ;;
    ort-utt) ortdec utt ;;
  esac
done
echo ALLDONE
