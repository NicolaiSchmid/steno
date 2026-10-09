#!/usr/bin/env zsh
# The CoreML measurements of A2 in .plans/2026-10-07-stable-promotion.md, on
# a Mac with the Swift app's Parakeet models: FLEURS German cat/ and the
# calibration corpus through `steno-coreml-parity` built at two revisions,
# each scored against the Swift transcripts (the harness table) and FLEURS
# against its references (spikes/onnx-speech/fleurs/score_fleurs.py, the
# normaliser of the gate).
#
#   scripts/coreml-parity-forge.sh <base-ref> <head-ref> [variant...]
#
# A variant is a label and environment switches, `label:VAR=value,VAR=value`,
# run on the head revision with scripts/coreml-parity-variants.py applied
# (its docstring lists the switches), for example
# `coreml-merge:STENO_A2_COREML_MERGE=1`.
#
# Environment:
#   STENO_FLEURS_DIR            holds cat/*.wav, cat/*.ref.txt and the Swift
#                               transcripts in cat/bakeoff/ (default
#                               ~/steno-spikes/fleurs)
#   STENO_CALIBRATION_CORPUS    the corpus WAVs (default ~/steno-spikes/corpus)
#   STENO_CALIBRATION_BASELINE  their Swift transcripts (default
#                               ~/steno-spikes/baseline-bakeoff)
#   SCRATCH                     clones, target dirs and outputs (default
#                               ~/steno-coreml-parity); delete it afterwards
#
# Nothing here is audio or a transcript; the outputs stay in SCRATCH. Every
# build runs with CARGO_BUILD_JOBS=4 under nice, and every run prints the
# load average, because the decode loop's cost is CPU scheduling.
set -eu
(( $# >= 2 )) || { print -u2 "usage: $0 <base-ref> <head-ref> [label:VAR=value,...]..."; exit 2 }
base=$1 head=$2; shift 2
fleurs=${STENO_FLEURS_DIR:-$HOME/steno-spikes/fleurs}
corpus=${STENO_CALIBRATION_CORPUS:-$HOME/steno-spikes/corpus}
baseline=${STENO_CALIBRATION_BASELINE:-$HOME/steno-spikes/baseline-bakeoff}
scratch=${SCRATCH:-$HOME/steno-coreml-parity}
repo=$(git -C "${0:A:h}" rev-parse --show-toplevel)
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0

mkdir -p $scratch
[[ -d $scratch/repo ]] || git clone -q $repo $scratch/repo
git -C $scratch/repo fetch -q origin

# build <label> <ref> [patch]: a worktree at <ref> (as this repository
# names it) and its parity binary.
build() {
  local label=$1 ref=$(git -C $repo rev-parse --verify "$2^{commit}") tree=$scratch/$1
  [[ -d $tree ]] || git -C $scratch/repo worktree add -q --detach $tree $ref
  git -C $tree checkout -q -f --detach $ref && git -C $tree clean -qfd crates
  [[ ${3:-} == patch ]] && python3 $repo/scripts/coreml-parity-variants.py $tree
  (cd $tree && CARGO_TARGET_DIR=$scratch/target-$label nice -n 19 \
    cargo build --release -q -p steno-speech-coreml --bin steno-coreml-parity)
}

# run <label> <binary>: FLEURS cat/ and the corpus, with the load around each.
run() {
  local label=$1 bin=$2 out=$scratch/out/$1
  mkdir -p $out
  print "## $label (load $(sysctl -n vm.loadavg))"
  nice -n 19 $bin --out $out/fleurs $fleurs/cat $fleurs/cat/bakeoff > $out/fleurs.md
  nice -n 19 $bin --out $out/corpus $corpus $baseline > $out/corpus.md
  print "FLEURS against Swift: $(tail -1 $out/fleurs.md)"
  print "Corpus against Swift: $(tail -1 $out/corpus.md)"
  python3 $repo/spikes/onnx-speech/fleurs/score_fleurs.py $fleurs/cat --bakeoff $out/fleurs rust | grep '^| rust'
  print "(load after $(sysctl -n vm.loadavg))"
}

build base $base
build head $head
run base $scratch/target-base/release/steno-coreml-parity
run head $scratch/target-head/release/steno-coreml-parity
print "## Swift"
python3 $repo/spikes/onnx-speech/fleurs/score_fleurs.py $fleurs/cat --bakeoff $fleurs/cat/bakeoff parakeet-v3 | grep '^| parakeet-v3'

if (( $# > 0 )); then
  build variants $head patch
  for variant in "$@"; do
    label=${variant%%:*}
    switches=(${(s:,:)${variant#*:}})
    (export $switches; run $label $scratch/target-variants/release/steno-coreml-parity)
  done
fi
