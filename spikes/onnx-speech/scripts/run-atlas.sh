#!/bin/sh
# ASR over the whole corpus, then diarization on a subset; load is recorded by the binary.
export LD_LIBRARY_PATH=/nix/store/hngmi01i8wgi25a0byrxcn4ysz5j79mw-gcc-15.2.0-lib/lib
cd /tmp/steno-spikes/onnx
B=./onnx-speech/target/release/onnx-speech-spike
uptime
$B --models models --corpus /tmp/steno-spikes/corpus --out out-atlas --threads 4 --tag atlas-t4 --skip-diar
uptime
$B --models models --corpus /tmp/steno-spikes/corpus --out out-atlas --threads 12 --tag atlas-t12 --skip-diar
uptime
$B --models models --corpus /tmp/steno-spikes/corpus --out out-atlas --threads 4 --tag atlas-t4-c20 --skip-diar --chunk-seconds 20
uptime
$B --models models --corpus /tmp/steno-spikes/corpus --out out-atlas --threads 12 --tag atlas-t12-diar --skip-asr --thresholds 0.5,0.7,0.8,0.9 --files bfbeef67,2d7b9aba
uptime
echo ALLDONE
