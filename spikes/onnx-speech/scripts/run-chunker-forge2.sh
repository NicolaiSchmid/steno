#!/bin/sh
# Second half of the spike D matrix, after the empty-decode recovery was added to
# stage 3: the two other chunk targets (chunker only), stage 3 with language and
# empty-decode voting at 4 threads, and the same at 10 threads.
cd ~/steno-spikes/chunker
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
B=./target/release/onnx-speech-spike
SLID=models/sherpa-onnx-whisper-tiny
run() { echo "=== $*"; uptime; DYLD_LIBRARY_PATH=$L $B --models models --corpus ~/steno-spikes/corpus --out out --skip-diar "$@"; }
run --threads 4 --tag s4-t25 --chunker vad --target-seconds 25 --slid-dir $SLID --vote
run --threads 4 --tag s1-t15 --chunker vad --target-seconds 15
run --threads 4 --tag s1-t30 --chunker vad --target-seconds 30
run --threads 10 --tag s4-t25-x10 --chunker vad --target-seconds 25 --slid-dir $SLID --vote
uptime
echo ALLDONE
