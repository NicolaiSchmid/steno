#!/bin/sh
cd ~/steno-spikes/onnx
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
B=./onnx-speech/target/release/onnx-speech-spike
while ps -eo args | grep -q "[t]ag forge-t10-diar "; do sleep 10; done
run() { echo "=== $*"; uptime; DYLD_LIBRARY_PATH=$L $B --models models --corpus ~/steno-spikes/corpus --out out-forge "$@"; }
run --threads 4 --tag forge-t4-c20 --skip-diar --chunk-seconds 20
run --threads 4 --tag forge-t4-coreml --skip-diar --provider coreml --files bfbeef67,5ea9e7e8
run --threads 10 --tag forge-t10 --skip-diar --files bfbeef67,5ea9e7e8
uptime
echo ALLDONE
