#!/usr/bin/env zsh
#
# Uploads Steno's fp32 ONNX export of Parakeet TDT 0.6B v3 to a Hugging Face
# model repository, with the CC-BY-4.0 attribution the licence requires, and
# prints the commit to pin in the manifest. GitHub release assets cap at
# 2 GB a file and encoder.weights is 2.4 GB, so this asset lives on Hugging
# Face; Silero VAD and the diarization models stay on the sherpa-onnx
# GitHub releases (crates/steno-speech/src/model_store.rs, crates/steno-diarize).
#
# Usage: tools/upload-models.sh [--repo <owner/name>] [--models <store root>]
#        [--private] [--dry-run]
#
#   --repo     the target repository; default NicolaiSchmid/steno-models,
#              a placeholder until the plan's parity list settles the account
#   --models   a store root holding parakeet-tdt-0.6b-v3-fp32/ (the files
#              spikes/onnx-speech/export/ writes); default $STENO_MODELS_DIR
#   --private  create the repository private if it does not exist yet
#   --dry-run  verify and stage, print the upload commands, upload nothing
#
# Needs huggingface-cli (pip install -U "huggingface_hub[cli]") signed in
# with a write token (huggingface-cli login, or HF_TOKEN in the
# environment), curl and sha256sum or shasum. The repository is created on
# the first upload.
#
# What lands in the repository:
#   parakeet-tdt-0.6b-v3-fp32/{encoder.onnx,encoder.weights,decoder.onnx,joiner.onnx,tokens.txt}
#   README.md       the model card, licence cc-by-4.0, base model named
#   ATTRIBUTION.md  the CC-BY-4.0 credit: creator, source, licence link and
#                   the changes made (ONNX conversion, longer position table)
# The layout is <asset id>/<file name>, the same as a Steno models root, so
# `huggingface-cli download` output can be served as a mirror unchanged.
#
# Afterwards set PARAKEET_V3_FP32_REVISION in
# crates/steno-speech/src/model_store.rs to the printed commit; the manifest
# then points at https://huggingface.co/<repo>/resolve/<commit>/<path>.

set -euo pipefail

repo_root="${0:A:h:h}"
manifest="$repo_root/crates/steno-speech/src/model_store.rs"
repo="NicolaiSchmid/steno-models"
models="${STENO_MODELS_DIR:-}"
private=()
dry_run=0
asset="parakeet-tdt-0.6b-v3-fp32"
files=(encoder.onnx encoder.weights decoder.onnx joiner.onnx tokens.txt)

while (( $# > 0 )); do
	case "$1" in
		--repo) repo="$2"; shift 2 ;;
		--models) models="$2"; shift 2 ;;
		--private) private=(--private); shift ;;
		--dry-run) dry_run=1; shift ;;
		-h|--help) sed -n '3,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
		*) print -u2 "unknown argument: $1"; exit 2 ;;
	esac
done

[[ -n "$models" ]] || { print -u2 "no models root: pass --models or set STENO_MODELS_DIR"; exit 2; }
directory="$models/$asset"
[[ -d "$directory" ]] || { print -u2 "$directory: not a directory"; exit 1; }
[[ "$repo" == */* ]] || { print -u2 "--repo must be <owner>/<name>"; exit 2; }

if (( $+commands[sha256sum] )); then
	sha256() { sha256sum "$1" | cut -d' ' -f1; }
else
	sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

# The checksums and sizes come from the Rust manifest, the one source of
# truth: a hosted copy must match them or the manifest changes with it.
print "verifying $directory against $manifest"
for name in $files; do
	line="$(grep -E "file\(\"$name\"" "$manifest" || true)"
	expected_sha="$(print -r -- "$line" | sed -n 's/.*"\([0-9a-f]\{64\}\)".*/\1/p')"
	expected_size="$(print -r -- "$line" | sed -n 's/.*, \([0-9_]*\)),.*/\1/p' | tr -d _)"
	[[ -n "$expected_sha" && -n "$expected_size" ]] || { print -u2 "$name: not in the manifest"; exit 1; }
	file="$directory/$name"
	[[ -f "$file" ]] || { print -u2 "$file: missing"; exit 1; }
	size="$(wc -c < "$file" | tr -d ' ')"
	[[ "$size" == "$expected_size" ]] || { print -u2 "$name: $size bytes, the manifest says $expected_size"; exit 1; }
	actual_sha="$(sha256 "$file")"
	[[ "$actual_sha" == "$expected_sha" ]] || { print -u2 "$name: sha256 $actual_sha, the manifest says $expected_sha"; exit 1; }
	print "  $name  $size bytes  ok"
done

staging="$(mktemp -d "${TMPDIR:-/tmp}/steno-models.XXXXXX")"
trap 'command rm -rf "$staging"' EXIT

cat > "$staging/ATTRIBUTION.md" <<ATTRIBUTION
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

cat > "$staging/README.md" <<README
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
Linux and Windows, where it runs speech recognition through ONNX Runtime.

| Path | What |
|------|------|
| \`$asset/\` | Parakeet TDT 0.6B v3, fp32 ONNX export: \`encoder.onnx\` with \`encoder.weights\`, \`decoder.onnx\`, \`joiner.onnx\`, \`tokens.txt\` |

Steno pins a commit of this repository and checks every file's size and
SHA-256 against its manifest (\`crates/steno-speech/src/model_store.rs\`).

Licence: CC BY 4.0, a derivative of NVIDIA's Parakeet TDT 0.6B v3; see
[ATTRIBUTION.md](ATTRIBUTION.md).
README

uploads=(
	"huggingface-cli upload $repo $directory $asset --repo-type model ${private[*]} --commit-message 'Parakeet TDT 0.6B v3 fp32 ONNX export'"
	"huggingface-cli upload $repo $staging/ATTRIBUTION.md ATTRIBUTION.md --repo-type model --commit-message 'CC-BY-4.0 attribution'"
	"huggingface-cli upload $repo $staging/README.md README.md --repo-type model --commit-message 'Model card'"
)

if (( dry_run )); then
	print "\ndry run, would run:"
	for upload in $uploads; do print "  $upload"; done
	print "\nstaged ATTRIBUTION.md:\n"
	cat "$staging/ATTRIBUTION.md"
	exit 0
fi

(( $+commands[huggingface-cli] )) || { print -u2 "huggingface-cli not found: pip install -U 'huggingface_hub[cli]'"; exit 1; }
huggingface-cli upload "$repo" "$directory" "$asset" --repo-type model $private \
	--commit-message "Parakeet TDT 0.6B v3 fp32 ONNX export"
huggingface-cli upload "$repo" "$staging/ATTRIBUTION.md" ATTRIBUTION.md --repo-type model \
	--commit-message "CC-BY-4.0 attribution"
huggingface-cli upload "$repo" "$staging/README.md" README.md --repo-type model \
	--commit-message "Model card"

auth=()
[[ -n "${HF_TOKEN:-}" ]] && auth=(-H "Authorization: Bearer $HF_TOKEN")
revision="$(curl -fsS $auth "https://huggingface.co/api/models/$repo/revision/main" \
	| sed -n 's/.*"sha":"\([0-9a-f]\{40\}\)".*/\1/p' | head -n 1)"
[[ -n "$revision" ]] || { print -u2 "uploaded, but could not read the commit of $repo; look it up on the repository page"; exit 1; }

print "\nuploaded $repo at $revision"
print "set in crates/steno-speech/src/model_store.rs:"
print "  pub const PARAKEET_V3_FP32_REVISION: Option<&str> = Some(\"$revision\");"
