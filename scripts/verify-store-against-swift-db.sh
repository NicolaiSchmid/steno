#!/usr/bin/env bash
#
# Cross-check of the Rust store against a database the Swift app wrote:
# copies the database to a temporary directory (the original is never
# opened, let alone written), opens the copy with steno-core and prints row
# counts and the first meeting's fields. Run on a machine that has such a
# database; nothing it prints belongs in the repo.
#
# Usage: scripts/verify-store-against-swift-db.sh <path to steno.sqlite>

set -euo pipefail

if [[ $# -ne 1 ]]; then
	echo "usage: $0 <path to steno.sqlite>" >&2
	exit 2
fi

source_db="$1"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cp "$source_db" "$tmp/steno.sqlite"
for suffix in -wal -shm; do
	[[ -f "$source_db$suffix" ]] && cp "$source_db$suffix" "$tmp/steno.sqlite$suffix"
done

cd "$repo_root"
cargo run -q -p steno-core --example inspect -- "$tmp/steno.sqlite"
