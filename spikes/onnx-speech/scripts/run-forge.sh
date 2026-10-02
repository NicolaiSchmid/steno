#!/bin/sh
cd ~/steno-spikes/onnx
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
B=./onnx-speech/target/release/onnx-speech-spike
run() { echo "=== $*"; uptime; DYLD_LIBRARY_PATH=$L $B --models models --corpus ~/steno-spikes/corpus --out out-forge "$@"; }
run --threads 4 --tag forge-t4 --skip-diar
run --threads 10 --tag forge-t10-diar --skip-asr --thresholds 0.7,0.8,0.9,0.95
run --threads 10 --tag forge-t10 --skip-diar
run --threads 4 --tag forge-t4-coreml --skip-diar --provider coreml
run --threads 4 --tag forge-t4-c20 --skip-diar --chunk-seconds 20
run --threads 10 --tag forge-t10-diar-wespeaker --skip-asr --thresholds 0.8,0.9,0.95 --embedding wespeaker_en_voxceleb_CAM++.onnx --files bfbeef67,2d7b9aba,c0cd3671
uptime
echo ALLDONE
