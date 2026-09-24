#!/usr/bin/env bash
#
# Bootstraps a fresh git worktree so it is usable without manual steps:
# copies local env files from the main checkout and installs the mobile
# app's dependencies when its lockfile changed. Invoked by
# .githooks/post-checkout (enable once: git config core.hooksPath .githooks).
# Safe to run by hand from any checkout; it is a no-op in the main checkout.

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
common_dir="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"

if [[ -z "$repo_root" || -z "$common_dir" ]]; then
	exit 0
fi

common_root="$(cd "$common_dir/.." && pwd -P)"

# The main checkout is the source of truth for local env files.
if [[ "$repo_root" == "$common_root" ]]; then
	exit 0
fi

copy_env_file() {
	local name="$1"
	local source_path="$common_root/$name"
	local target_path="$repo_root/$name"

	if [[ -f "$source_path" && ! -e "$target_path" ]]; then
		mkdir -p "$(dirname "$target_path")"
		cp "$source_path" "$target_path"
		echo "[worktree-setup] copied $name"
	fi
}

copy_env_file "mobile/.env"
copy_env_file "mobile/.env.local"

if [[ ! -f "$repo_root/mobile/package.json" ]]; then
	exit 0
fi

if ! command -v pnpm >/dev/null 2>&1; then
	echo "[worktree-setup] pnpm not found; skipped mobile install" >&2
	exit 0
fi

stamp_path="$(git rev-parse --git-path worktree-bootstrap.lockhash)"
current_lock_hash=""
previous_lock_hash=""

if [[ -f "$repo_root/mobile/pnpm-lock.yaml" ]]; then
	current_lock_hash="$(shasum -a 256 "$repo_root/mobile/pnpm-lock.yaml" | awk '{print $1}')"
fi

if [[ -f "$stamp_path" ]]; then
	previous_lock_hash="$(<"$stamp_path")"
fi

needs_install=0
reason=""

if [[ ! -d "$repo_root/mobile/node_modules" ]]; then
	needs_install=1
	reason="mobile/node_modules missing"
elif [[ "$current_lock_hash" != "$previous_lock_hash" ]]; then
	needs_install=1
	reason="mobile/pnpm-lock.yaml changed"
fi

if [[ "$needs_install" -eq 0 ]]; then
	exit 0
fi

echo "[worktree-setup] running pnpm install in mobile/ ($reason)"
(cd "$repo_root/mobile" && pnpm install)
printf '%s\n' "$current_lock_hash" >"$stamp_path"
