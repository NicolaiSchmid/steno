#!/usr/bin/env bash
#
# Uploads Steno's fp32 ONNX export of Parakeet TDT 0.6B v3 to a Hugging Face
# model repository, with the CC-BY-4.0 attribution the licence requires, and
# prints the commit to pin in the manifest. GitHub release assets cap at
# 2 GB a file and encoder.weights is 2.4 GB, so this asset lives on Hugging
# Face; Silero VAD stays on the sherpa-onnx GitHub release
# (crates/steno-speech/src/model_store.rs) and the diarizer's two models on
# Hugging Face and a sherpa-onnx GitHub release
# (crates/steno-diarize/src/models.rs). A mirror replaces all of these hosts,
# so it serves a whole store root: parakeet-tdt-0.6b-v3-fp32/, silero-vad/
# and diarization/.
#
# Usage: scripts/upload-models.sh [--repo <owner/name>] [--models <store root>]
#        [--private] [--dry-run]
#
#   --repo     the target repository; default nicolaischmid/steno-models,
#              the one STENO_MODELS_REPO names
#   --models   a store root holding parakeet-tdt-0.6b-v3-fp32/ (the files
#              spikes/onnx-speech/export/ writes); default
#              $STENO_MODELS_DIR/onnx, the store root inside the models
#              directory
#   --private  create the repository private if it does not exist yet
#   --dry-run  verify and stage, print the upload commands, upload nothing
#
# Needs the Hugging Face CLI, hf (pip install -U huggingface_hub), signed in
# with a write token (hf auth login, or HF_TOKEN in the environment), curl
# and sha256sum or shasum. The repository is created on the first upload.
#
# What lands in the repository, and nothing else from the store root (no
# partial downloads, no lock files, no Finder files):
#   parakeet-tdt-0.6b-v3-fp32/{encoder.onnx,encoder.weights,decoder.onnx,joiner.onnx,tokens.txt}
#   README.md       the model card, licence cc-by-4.0, base model named
#   ATTRIBUTION.md  the CC-BY-4.0 credit: creator, source, licence link and
#                   the changes made (ONNX conversion, longer position table)
# The layout is <asset id>/<file name>, the same as a Steno store root, so
# the output of `hf download <repo> --revision <commit> --local-dir <dir>`
# is the Parakeet part of a mirror; add silero-vad/ and diarization/ from an
# installed store root before serving it.
#
# Afterwards set PARAKEET_V3_FP32_REVISION in
# crates/steno-speech/src/model_store.rs to the printed commit (and
# STENO_MODELS_REPO to the repository, if it changed); the manifest points
# at https://huggingface.co/<repo>/resolve/<commit>/<path>.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
manifest="$repo_root/crates/steno-speech/src/model_store.rs"
repo="nicolaischmid/steno-models"
models="${STENO_MODELS_DIR:+$STENO_MODELS_DIR/onnx}"
private=()
dry_run=0
asset="parakeet-tdt-0.6b-v3-fp32"
files=(encoder.onnx encoder.weights decoder.onnx joiner.onnx tokens.txt)

fail() {
	echo "$2" >&2
	exit "$1"
}

while (($# > 0)); do
	case "$1" in
	--repo)
		repo="${2:?--repo needs a value}"
		shift 2
		;;
	--models)
		models="${2:?--models needs a value}"
		shift 2
		;;
	--private)
		private=(--private)
		shift
		;;
	--dry-run)
		dry_run=1
		shift
		;;
	-h | --help)
		awk 'NR > 2 && /^#/ { sub(/^# ?/, ""); print; next } NR > 2 { exit }' "$0"
		exit 0
		;;
	*) fail 2 "unknown argument: $1" ;;
	esac
done

[[ -n "$models" ]] || fail 2 "no store root: pass --models or set STENO_MODELS_DIR"
directory="$models/$asset"
[[ -d "$directory" ]] || fail 1 "$directory: not a directory"
[[ "$repo" == */* ]] || fail 2 "--repo must be <owner>/<name>"

if command -v sha256sum >/dev/null; then
	sha256() { sha256sum "$1" | cut -d' ' -f1; }
else
	sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

# The checksums and sizes come from the Rust manifest, the one source of
# truth: a hosted copy must match them or the manifest changes with it.
echo "verifying $directory against $manifest"
for name in "${files[@]}"; do
	line="$(grep -E "file\(\"$name\"" "$manifest" || true)"
	expected_sha="$(sed -n 's/.*"\([0-9a-f]\{64\}\)".*/\1/p' <<<"$line")"
	expected_size="$(sed -n 's/.*, \([0-9_]*\)),.*/\1/p' <<<"$line" | tr -d _)"
	[[ -n "$expected_sha" && -n "$expected_size" ]] || fail 1 "$name: not in the manifest"
	file="$directory/$name"
	[[ -f "$file" ]] || fail 1 "$file: missing"
	size="$(wc -c <"$file" | tr -d ' ')"
	[[ "$size" == "$expected_size" ]] || fail 1 "$name: $size bytes, the manifest says $expected_size"
	actual_sha="$(sha256 "$file")"
	[[ "$actual_sha" == "$expected_sha" ]] || fail 1 "$name: sha256 $actual_sha, the manifest says $expected_sha"
	echo "  $name  $size bytes  ok"
done

staging="$(mktemp -d "${TMPDIR:-/tmp}/steno-models.XXXXXX")"
trap 'rm -rf "$staging"' EXIT

cat >"$staging/ATTRIBUTION.md" <<ATTRIBUTION
# Attribution

The files under \`$asset/\` are a derivative of **Parakeet TDT 0.6B v3** by
NVIDIA, <https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3>, licensed under
the Creative Commons Attribution 4.0 International licence (CC BY 4.0),
<https://creativecommons.org/licenses/by/4.0/>.

Changes made: the NeMo checkpoint was exported to ONNX in fp32 by
\`spikes/onnx-speech/export/\` in <https://github.com/NicolaiSchmid/steno>
(torch 2.14.1, NeMo 3.0.0): encoder (with its weights in the external data
file \`encoder.weights\`), decoder and joiner as separate graphs, the
encoder's relative position table extended to 10000 frames, and the
vocabulary written as \`tokens.txt\`. The weights are NVIDIA's, unchanged;
nothing was retrained or quantised.

This derivative is distributed under the same licence, CC BY 4.0. There
is no endorsement by NVIDIA.
ATTRIBUTION

cat >"$staging/README.md" <<README
---
license: cc-by-4.0
base_model: nvidia/parakeet-tdt-0.6b-v3
library_name: onnx
pipeline_tag: automatic-speech-recognition
tags:
  - onnx
  - parakeet
  - tdt
  - steno
---

# Steno speech models

The models [Steno](https://github.com/NicolaiSchmid/steno) downloads on
Linux and Windows, and on a Mac with the ONNX fallback, where it runs
speech recognition through ONNX Runtime.

| Path | What |
|------|------|
| \`$asset/\` | Parakeet TDT 0.6B v3, fp32 ONNX export: \`encoder.onnx\` with \`encoder.weights\`, \`decoder.onnx\`, \`joiner.onnx\`, \`tokens.txt\` |

Steno pins a commit of this repository and checks every file's size and
SHA-256 against its manifest (\`crates/steno-speech/src/model_store.rs\`).

Licence: CC BY 4.0, a derivative of NVIDIA's Parakeet TDT 0.6B v3; see
[ATTRIBUTION.md](ATTRIBUTION.md).
README

# Prints the command on a dry run, runs it otherwise.
run() {
	if ((dry_run)); then
		printf ' %q' "$@"
		echo
	else
		"$@"
	fi
}

if ((dry_run)); then
	printf '\ndry run, would run:\n'
else
	command -v hf >/dev/null || fail 1 "hf not found: pip install -U huggingface_hub"
fi
# File by file: only the verified files go up, never a partial download
# or anything else in the directory. The first upload creates the
# repository.
for name in "${files[@]}"; do
	run hf upload "$repo" "$directory/$name" "$asset/$name" --repo-type model \
		${private[@]+"${private[@]}"} --commit-message "Parakeet TDT 0.6B v3 fp32 ONNX export: $name"
	private=()
done
run hf upload "$repo" "$staging/ATTRIBUTION.md" ATTRIBUTION.md --repo-type model \
	--commit-message "CC-BY-4.0 attribution"
run hf upload "$repo" "$staging/README.md" README.md --repo-type model \
	--commit-message "Model card"

if ((dry_run)); then
	printf '\nstaged ATTRIBUTION.md:\n\n'
	cat "$staging/ATTRIBUTION.md"
	exit 0
fi

# The token goes to curl on stdin, not its command line, where ps shows it.
auth=()
[[ -n "${HF_TOKEN:-}" ]] && auth=(-H @-)
revision="$(printf 'Authorization: Bearer %s\n' "${HF_TOKEN:-}" |
	curl -fsS ${auth[@]+"${auth[@]}"} "https://huggingface.co/api/models/$repo/revision/main" |
	sed -n 's/.*"sha":"\([0-9a-f]\{40\}\)".*/\1/p' | head -n 1)"
[[ -n "$revision" ]] || fail 1 "uploaded, but could not read the commit of $repo; look it up on the repository page"

printf '\nuploaded %s at %s\n' "$repo" "$revision"
echo "set in crates/steno-speech/src/model_store.rs:"
echo "  pub const PARAKEET_V3_FP32_REVISION: Option<&str> = Some(\"$revision\");"
