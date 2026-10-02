#!/bin/sh
# Spike D matrix on Forge: stage 0 baseline, stage 1 chunker at three target lengths,
# stages 2+3 (SLID lane prior + segment voting) at 4 threads, and the final
# configuration at 10 threads. Run under nohup; DYLD_LIBRARY_PATH must be set in
# the final command because SIP strips it from /bin/sh.
cd ~/steno-spikes/chunker
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
B=./target/release/onnx-speech-spike
SLID=models/sherpa-onnx-whisper-tiny
run() { echo "=== $*"; uptime; DYLD_LIBRARY_PATH=$L $B --models models --corpus ~/steno-spikes/corpus --out out --skip-diar "$@"; }
run --threads 4 --tag s0-c20 --chunk-seconds 20
run --threads 4 --tag s1-t25 --chunker vad --target-seconds 25
run --threads 4 --tag s3-t25 --chunker vad --target-seconds 25 --slid-dir $SLID --vote
run --threads 4 --tag s1-t15 --chunker vad --target-seconds 15
run --threads 4 --tag s1-t30 --chunker vad --target-seconds 30
run --threads 10 --tag s3-t25-x10 --chunker vad --target-seconds 25 --slid-dir $SLID --vote
uptime
echo ALLDONE
