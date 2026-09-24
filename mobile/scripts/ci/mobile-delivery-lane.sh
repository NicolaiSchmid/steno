#!/usr/bin/env bash
#
# Decide which delivery lane a mobile change takes on one platform: OTA update
# or native build. Shared by `.github/workflows/mobile-cd.yml` (repo root) (the `decide`
# job, which acts on the answer) and `.github/workflows/mobile-ci.yml` (the PR
# "delivery preview" comment, which only reports it), so the two can never
# disagree — a preview that says "OTA" while CD rebuilds is worse than none.
#
# Usage (from anywhere; it cds into mobile/ itself):
#   mobile/scripts/ci/mobile-delivery-lane.sh --platform <ios|android> [--mode auto|update|build]
#
# How it decides:
#   1. Compute the Expo native fingerprint for the platform with
#      `expo-updates fingerprint:generate` under APP_VARIANT=production — the
#      exact algorithm expo-updates uses for the `runtimeVersion` fingerprint
#      policy, so the gate cannot disagree with real OTA delivery.
#   2. Compare it with the MOST RECENTLY CREATED `<platform>-fp-*` tag. CD
#      pushes `<platform>-fp-<hash>` (with -f) at the commit it built after a
#      native build has been submitted to the store, so the newest tag is the
#      runtime the store binary runs right now. Equal → OTA is safe. Anything
#      else → build: no tag yet (first release), a new fingerprint, or a
#      fingerprint that WAS released before but is not the current binary's
#      (a native revert) — an OTA with the old runtime would be rejected by
#      everyone on the current binary and nothing would reach them.
#      Local `eas build --local` binaries are NOT registered on EAS, which is
#      why a git tag, not the EAS API, is the record.
#   3. `--mode update|build` forces the lane (workflow_dispatch); `auto`
#      (default) uses the tag.
#
# Output: `key=value` lines on stdout, prefixed with the platform —
#   ios_fingerprint=<hash>   ios_tag=ios-fp-<hash>   ios_action=update|build
#   ios_reason=<one line, human readable>
# and, when $GITHUB_OUTPUT is set, the same lines appended there so the calling
# step exposes them as outputs. Human-readable progress goes to stderr.
#
# Requirements: run after `pnpm install` (needs `expo-updates` resolvable from
# mobile/) and with tags fetched (`actions/checkout` with fetch-depth: 0).

set -euo pipefail

PLATFORM=""
MODE="auto"

while [ $# -gt 0 ]; do
	case "$1" in
		--platform)
			PLATFORM="${2:-}"
			shift 2
			;;
		--mode)
			MODE="${2:-}"
			shift 2
			;;
		-h | --help)
			sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
			exit 0
			;;
		*)
			echo "mobile-delivery-lane: unknown argument '$1'" >&2
			exit 2
			;;
	esac
done

case "$PLATFORM" in
	ios | android) ;;
	*)
		echo "mobile-delivery-lane: --platform must be ios or android (got '${PLATFORM}')" >&2
		exit 2
		;;
esac

case "$MODE" in
	auto | update | build) ;;
	*)
		echo "mobile-delivery-lane: --mode must be auto, update or build (got '${MODE}')" >&2
		exit 2
		;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MOBILE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$MOBILE_DIR"

# APP_VARIANT=production: app.config.ts resolves the production name, scheme
# and bundle id from it, and all of those are hashed into the fingerprint. The
# development variant would compute a different (wrong) hash.
HASH="$(
	APP_VARIANT=production pnpm exec expo-updates fingerprint:generate --platform "$PLATFORM" \
		| node -e '
			const raw = require("fs").readFileSync(0, "utf8");
			const start = raw.indexOf("{");
			if (start < 0) { console.error("fingerprint:generate printed no JSON:\n" + raw); process.exit(1); }
			const { hash } = JSON.parse(raw.slice(start));
			if (typeof hash !== "string" || !/^[0-9a-f]{20,}$/.test(hash)) {
				console.error("fingerprint:generate returned an unexpected hash: " + JSON.stringify(hash));
				process.exit(1);
			}
			process.stdout.write(hash);
		'
)"

TAG="${PLATFORM}-fp-${HASH}"

# The newest build tag for this platform. Lightweight tags sort by the tagged
# commit's committer date; CD tags the commit it built (and re-tags with -f
# when a historical fingerprint is re-released), so "newest" is the binary
# that is in the store now. `sed -n 1p` rather than `head -1`: sed drains its
# input, so pipefail cannot trip on an early close.
LATEST_TAG="$(
	git for-each-ref --sort=-creatordate --format='%(refname:short)' 		"refs/tags/${PLATFORM}-fp-*" | sed -n '1p'
)"

if [ "$MODE" = "build" ] || [ "$MODE" = "update" ]; then
	ACTION="$MODE"
	REASON="forced via workflow_dispatch (mode=$MODE)"
elif [ -z "$LATEST_TAG" ]; then
	ACTION="build"
	REASON="no ${PLATFORM}-fp-* tag yet → first native build"
elif [ "$LATEST_TAG" = "$TAG" ]; then
	ACTION="update"
	REASON="newest build tag $TAG matches this fingerprint → the store binary runs it → OTA update"
elif git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
	ACTION="build"
	REASON="fingerprint $HASH was released before ($TAG) but the newest binary is $LATEST_TAG → an OTA with this runtime would not reach it → store build"
else
	ACTION="build"
	REASON="no tag $TAG (newest is $LATEST_TAG) → native layer changed → store build"
fi

echo "[$PLATFORM] fingerprint $HASH → $ACTION ($REASON)" >&2

emit() {
	printf '%s_fingerprint=%s\n%s_tag=%s\n%s_action=%s\n%s_reason=%s\n' \
		"$PLATFORM" "$HASH" "$PLATFORM" "$TAG" "$PLATFORM" "$ACTION" "$PLATFORM" "$REASON"
}

emit
if [ -n "${GITHUB_OUTPUT:-}" ]; then
	emit >>"$GITHUB_OUTPUT"
fi
