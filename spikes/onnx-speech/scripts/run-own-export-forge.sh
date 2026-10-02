#!/bin/sh
# Spike E matrix on Forge: the stock int8 export as control, then each own export under
# ~/steno-spikes/models-own/<variant>, all with the spike D final configuration (VAD
# chunker, 25 s target, SLID lane prior, language + empty-decode voting, 4 threads).
# Run under nohup; DYLD_LIBRARY_PATH must be set in the final command because SIP
# strips it from /bin/sh. Usage: run-own-export-forge.sh [variant ...]
cd ~/steno-spikes/chunker
L=$(ls -d ~/Library/Caches/sherpa-rs/*/*/*/lib)
B=./target/release/onnx-speech-spike
SLID=models/sherpa-onnx-whisper-tiny
OWN=~/steno-spikes/models-own
run() { echo "=== $*"; uptime; DYLD_LIBRARY_PATH=$L $B --models models --corpus ~/steno-spikes/corpus --out out --skip-diar --threads 4 --chunker vad --target-seconds 25 --slid-dir $SLID --vote "$@"; }
for v in ${*:-stock fp32 int8 int8-pc int8-noattn}; do
  case $v in
    stock) run --tag e-stock ;;
    fp32) run --tag e-fp32 --asr-dir $OWN/fp32 --asr-files fp32 ;;
    *) run --tag e-$v --asr-dir $OWN/$v --asr-files int8 ;;
  esac
done
uptime
echo ALLDONE
