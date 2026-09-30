#!/usr/bin/env bash
#
# Bootstraps a fresh git worktree so it is usable without manual steps:
# copies local env files from the main checkout and installs the pnpm
# projects' dependencies (mobile/ and apps/macos/web/) when a lockfile
# changed. Invoked by .githooks/post-checkout (enable once: git config
# core.hooksPath .githooks). Safe to run by hand from any checkout; it is a
# no-op in the main checkout.

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

if ! command -v pnpm >/dev/null 2>&1; then
	echo "[worktree-setup] pnpm not found; skipped dependency install" >&2
	exit 0
fi

# One stamp per project, keyed by the lockfile hash, so a lockfile change in
# either project triggers only that project's install.
install_if_needed() {
	local project="$1"
	local dir="$repo_root/$project"
	local stamp_name="worktree-bootstrap-${project//\//-}.lockhash"

	if [[ ! -f "$dir/package.json" ]]; then
		return 0
	fi

	local stamp_path current_lock_hash="" previous_lock_hash=""
	stamp_path="$(git rev-parse --git-path "$stamp_name")"

	if [[ -f "$dir/pnpm-lock.yaml" ]]; then
		current_lock_hash="$(shasum -a 256 "$dir/pnpm-lock.yaml" | awk '{print $1}')"
	fi
	if [[ -f "$stamp_path" ]]; then
		previous_lock_hash="$(<"$stamp_path")"
	fi

	local reason=""
	if [[ ! -d "$dir/node_modules" ]]; then
		reason="$project/node_modules missing"
	elif [[ "$current_lock_hash" != "$previous_lock_hash" ]]; then
		reason="$project/pnpm-lock.yaml changed"
	fi
	if [[ -z "$reason" ]]; then
		return 0
	fi

	echo "[worktree-setup] running pnpm install in $project/ ($reason)"
	(cd "$dir" && pnpm install)
	printf '%s\n' "$current_lock_hash" >"$stamp_path"
}

install_if_needed "mobile"
install_if_needed "apps/macos/web"
