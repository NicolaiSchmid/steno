#!/usr/bin/env bash
# Checks .github/workflows/desktop-release.yml against what the stable
# plan's "Release mechanics" requires of it (S7 of
# .plans/2026-10-07-stable-promotion.md), the assertions of the Swift
# release workflow's tests among them: `v*` tags; the build number in the
# Mac bundle and checked against the `appcast` branch; the handoff item
# signed on tags only, by the one step that sees SPARKLE_PRIVATE_KEY, into
# its own artifact; `publish` makes a stable release public and "latest"
# with appcast.xml before the cask bump, and a pre-release bumps nothing;
# `handoff` needs `publish` behind the `appcast` environment, and stops
# unless that environment has a required reviewer; `contents: write` where
# a job writes. It also runs four steps as the workflow has them: `plan`'s
# version step over a scratch repository (the tag it accepts, the
# pre-release flag the stable-only steps read, the build number);
# "Publish the release" and the handoff job's environment check against a
# stub `gh`; and "Date the item" over a scratch `appcast` origin.
# rust-ci.yml runs it on Linux. Needs Python's PyYAML (the runner image
# has it; on NixOS, nix-shell -p 'python3.withPackages (p: [p.pyyaml])').
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workflow="$here/../../../.github/workflows/desktop-release.yml"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

python3 - "$workflow" "$scratch" <<'PY'
import fnmatch
import re
import sys

import yaml

path, scratch = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    text = handle.read()
workflow = yaml.safe_load(text)
jobs = workflow["jobs"]
failures = []


def check(condition, message):
    if not condition:
        failures.append(message)


def steps(job):
    return jobs[job]["steps"]


def step(job, name):
    found = [s for s in steps(job) if s.get("name") == name]
    check(len(found) == 1, "%s has %d steps named %r" % (job, len(found), name))
    return found[0] if found else {}


def index(job, name):
    names = [s.get("name") for s in steps(job)]
    return names.index(name) if name in names else -1


def checkout(job):
    return next(s for s in steps(job) if s.get("uses", "").startswith("actions/checkout@"))


# The trigger (D1): v* tags, never desktop-v* again.
on = workflow.get("on", workflow.get(True))
check(on["push"] == {"tags": ["v*"]}, "the push trigger is %r, expected tags ['v*']" % on["push"])
check("desktop-v$" not in text and "desktop-v*" not in text.split("\njobs:")[1], "a job still names desktop-v tags")

# Permissions: read by default, write only where a job writes.
check(workflow["permissions"] == {"contents": "read"}, "the default permissions are %r" % workflow["permissions"])
check(jobs["publish"]["permissions"].get("contents") == "write", "publish lacks contents: write")
check(
    jobs["handoff"]["permissions"] == {"actions": "read", "contents": "write"},
    "handoff's permissions are %r" % jobs["handoff"].get("permissions"),
)

# plan: the whole history, the outputs, the build against the branch.
check(checkout("plan").get("with", {}).get("fetch-depth") == 0, "plan does not check out the whole history")
for output in ("version", "prerelease", "build"):
    check(output in jobs["plan"]["outputs"], "plan does not output %s" % output)
check(index("plan", "Build number against the appcast branch") > 0, "plan does not check the build number")
build_check = step("plan", "Build number against the appcast branch")
check(build_check.get("env") == {"BUILD": "${{ steps.version.outputs.build }}"}, "plan's build check reads %r" % build_check.get("env"))
check(
    'handoff-appcast.py check-build "$RUNNER_TEMP/appcast.xml" "$BUILD"' in build_check.get("run", ""),
    "plan's build check does not run check-build on the branch's feed and the build",
)

# bundle: the whole history on macOS, the build number in CFBundleVersion
# through the configuration file Bundle reads too.
depth = checkout("bundle").get("with", {}).get("fetch-depth")
check(depth == "${{ matrix.name == 'macos' && '0' || '1' }}", "the bundle checkout's fetch-depth is %r" % depth)
# The secrets guard runs before any tool or build.
check(
    steps("bundle")[0] is checkout("bundle") and steps("bundle")[1].get("name") == "Check secrets",
    "the bundle job's first step after the checkout is %r, not Check secrets" % steps("bundle")[1].get("name"),
)
build_step = step("bundle", "Build")
check(build_step.get("env", {}).get("BUILD") == "${{ needs.plan.outputs.build }}", "Build does not get plan's build")
build = build_step.get("run", "")
check(
    'count="$(git rev-list --count HEAD)"\n' in build and '[ "$count" = "$BUILD" ] || {' in build,
    "Build does not require the macOS checkout's count to be plan's build",
)
check('\\"bundleVersion\\":\\"$BUILD\\"' in build, "Build does not set bundle.macOS.bundleVersion to the build")
check('> "$RUNNER_TEMP/tauri-configs"' in build, "Build does not write the configurations for Bundle")
check('< "$RUNNER_TEMP/tauri-configs"' in step("bundle", "Bundle").get("run", ""), "Bundle does not read Build's configurations")
check(
    'signed=(--signed --handoff "$BUILD")' in step("bundle", "Check the bundles").get("run", ""),
    "the macOS bundle check is not --signed --handoff with the build",
)

# The handoff item: tag runs only, after notarisation and the check, the
# only step that sees the key, into its own artifact.
item = step("bundle", "Handoff item")
tag_only = "matrix.name == 'macos' && github.event_name == 'push' && github.ref_type == 'tag'"
check(item.get("if") == tag_only, "the Handoff item runs on %r" % item.get("if"))
check(index("bundle", "Notarise the disk image") < index("bundle", "Handoff item"), "the item is signed before notarisation")
check(index("bundle", "Check the bundles") < index("bundle", "Handoff item"), "the item is signed before the bundle check")
check(item.get("env", {}).get("SPARKLE_PRIVATE_KEY") == "${{ secrets.SPARKLE_PRIVATE_KEY }}", "the Handoff item does not get the key")
check('handoff-item.sh "$VERSION" "$BUILD" "$STENO_DMG"' in item.get("run", ""), "the Handoff item does not run handoff-item.sh on the image")
check(
    'echo "STENO_DMG=${dmgs[0]}" >> "$GITHUB_ENV"' in step("bundle", "Notarise the disk image").get("run", ""),
    "Notarise the disk image does not leave the image's path in STENO_DMG",
)
uses = re.findall(r"secrets\.SPARKLE_PRIVATE_KEY[^}]*", text)
check(
    sorted(uses) == sorted(["secrets.SPARKLE_PRIVATE_KEY ", "secrets.SPARKLE_PRIVATE_KEY != '' && 'set' || '' "]),
    "SPARKLE_PRIVATE_KEY reaches more than the Handoff item and the presence check: %r" % uses,
)
check(
    "SPARKLE_PRIVATE_KEY" in step("bundle", "Check secrets").get("run", ""),
    "the bundle job's Check secrets does not name SPARKLE_PRIVATE_KEY",
)
upload = step("bundle", "Upload the handoff item")
check(upload.get("if") == tag_only, "the item is uploaded on %r" % upload.get("if"))
check(upload.get("with", {}).get("name") == "sparkle-item", "the item's artifact is %r" % upload.get("with", {}).get("name"))
pattern = next(s for s in steps("assets") if s.get("uses", "").startswith("actions/download-artifact@"))["with"]["pattern"]
check(not fnmatch.fnmatch("sparkle-item", pattern), "assets would gather sparkle-item through %r" % pattern)

# assets: the URLs and the notes name v<version>.
check(
    "releases/download/v$VERSION\"" in step("assets", "Updater manifest").get("run", ""),
    "the manifest's base URL is not releases/download/v<version>",
)
check('release-notes.sh "$VERSION" "v$VERSION"' in step("assets", "Release notes").get("run", ""), "the summary's notes do not name v<version>")

# publish: tag runs only; a stable release public and latest with its
# appcast.xml, before the cask bump; a pre-release never latest.
publish = jobs["publish"]
check(publish["if"] == "github.event_name == 'push' && github.ref_type == 'tag'", "publish runs on %r" % publish["if"])
check(publish["outputs"].get("handoff_item") == "${{ steps.appcast.outputs.handoff_item }}", "publish does not output handoff_item")
stable = "needs.plan.outputs.prerelease == 'false'"
appcast = step("publish", "Appcast from the branch")
check(appcast.get("id") == "appcast" and appcast.get("if") == stable, "Appcast from the branch is not the stable-only step 'appcast'")
check("has-handoff-item" in appcast.get("run", ""), "publish does not ask whether the branch has the handoff item")
order = [index("publish", name) for name in ("Appcast from the branch", "Publish the release", "Update lanes", "Bump the Homebrew cask")]
check(order == sorted(order) and -1 not in order, "publish's steps are out of order: %r" % order)
for name in ("Bump the Homebrew cask", "Push the AUR bump", "Nix flake lines in the summary"):
    check(step("publish", name).get("if") == stable, "%r runs for a pre-release: %r" % (name, step("publish", name).get("if")))
check("bump-homebrew-cask.sh" in step("publish", "Bump the Homebrew cask").get("run", ""), "the cask step does not bump the cask")
check("aur-bump.sh" in step("publish", "Push the AUR bump").get("run", ""), "the AUR step does not run aur-bump.sh")
# Without its secret each script prints a notice and exits 0, so a missing
# mapping would skip the bump without a word.
for name, secret in (("Bump the Homebrew cask", "HOMEBREW_TAP_TOKEN"), ("Push the AUR bump", "AUR_SSH_PRIVATE_KEY")):
    env = step("publish", name).get("env")
    check(env == {secret: "${{ secrets.%s }}" % secret}, "%r gets %r, not %s" % (name, env, secret))
for name, job in jobs.items():
    if name != "publish":
        for s in job.get("steps", []):
            check("bump-homebrew-cask.sh" not in s.get("run", ""), "%s bumps the cask" % name)

# handoff: after publish, stable only, once, behind the environment.
handoff = jobs["handoff"]
check({"plan", "publish"} <= set(handoff["needs"]), "handoff needs %r" % handoff["needs"])
check(handoff.get("environment") == "appcast", "handoff runs in %r" % handoff.get("environment"))
condition = " ".join(handoff["if"].split())
check(
    condition == "needs.plan.outputs.prerelease == 'false' && (needs.publish.outputs.handoff_item == 'false' "
    "|| vars.HANDOFF_ITEM_REPLACE == needs.plan.outputs.version)",
    "handoff runs on %r" % condition,
)
names = [s.get("name") or s.get("uses") for s in steps("handoff")]
check(names[0] == "The appcast environment has a required reviewer", "handoff's first step is %r" % names[0])
dated = index("handoff", "Date the item")
pushed = index("handoff", "Put the item on the appcast branch")
asset = index("handoff", "The branch's feed as the release's appcast.xml")
check(0 < dated < pushed < asset, "handoff's steps are out of order: %r" % names)
dating = step("handoff", "Date the item")
check(dating.get("id") == "date", "Date the item is not the step 'date'")
check(
    dating.get("env") == {"VERSION": "${{ needs.plan.outputs.version }}", "HANDOFF_ITEM_REPLACE": "${{ vars.HANDOFF_ITEM_REPLACE }}"},
    "Date the item reads %r" % dating.get("env"),
)
push = step("handoff", "Put the item on the appcast branch")
check(push.get("if") == "steps.date.outputs.on_branch == 'false'", "the item goes on the branch again on a re-run: %r" % push.get("if"))
check(push.get("run") == 'apps/macos/scripts/publish-appcast.sh "$TAG" false', "the item goes on with %r" % push.get("run"))
check(push.get("env", {}).get("STENO_RELEASE_APPCAST", "").endswith("/sparkle-item/appcast.xml"), "publish-appcast.sh does not read sparkle-item")
check("--clobber" in step("handoff", "The branch's feed as the release's appcast.xml").get("run", ""), "the release's appcast.xml is not replaced")

# The steps the shell half runs, as written.
for name, script in (
    ("version-step.sh", next(s for s in steps("plan") if s.get("id") == "version")),
    ("publish-step.sh", step("publish", "Publish the release")),
    ("reviewer-step.sh", steps("handoff")[0]),
    ("date-step.sh", dating),
):
    with open(scratch + "/" + name, "w", encoding="utf-8") as handle:
        handle.write(script.get("run", "exit 1"))

if failures:
    print("\n".join("FAIL: " + failure for failure in failures))
    sys.exit(1)
PY

# The version step: a scratch repository with three commits, the version
# under [workspace.package] and the one script the step calls.
failures=0
fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}
repo="$scratch/repo"
mkdir -p "$repo/apps/desktop/scripts"
cp "$here/wix-version.sh" "$repo/apps/desktop/scripts/"
git -C "$repo" init --quiet
for n in 1 2 3; do
  git -C "$repo" -c user.name=test -c user.email=test@example.com commit --quiet --allow-empty -m "$n"
done

# version_step <workspace version> <ref type> <ref name>: the step's
# outputs, or its error.
version_step() {
  printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\nedition = "2024"\n' "$1" > "$repo/Cargo.toml"
  : > "$scratch/output"
  (cd "$repo" && GITHUB_OUTPUT="$scratch/output" GITHUB_REF_TYPE="$2" GITHUB_REF_NAME="$3" \
    bash -e "$scratch/version-step.sh" >"$scratch/error" 2>&1) || { cat "$scratch/error"; return 1; }
  cat "$scratch/output"
}
outputs="$(version_step 0.11.0-rc.1 tag v0.11.0-rc.1)" || fail "v0.11.0-rc.1 was refused: $outputs"
[[ "$outputs" == *$'\nprerelease=true\n'* ]] || fail "a candidate is not a pre-release, so it would bump the cask: $outputs"
[[ "$outputs" == *$'\nbuild=3'* ]] || fail "the build number is not the commit count: $outputs"
outputs="$(version_step 0.11.0 tag v0.11.0)" || fail "v0.11.0 was refused: $outputs"
[[ "$outputs" == *$'\nprerelease=false\n'* ]] || fail "0.11.0 is not a stable release: $outputs"
outputs="$(version_step 0.11.0 branch main)" || fail "a manual run was refused: $outputs"
if output="$(version_step 0.11.0 tag desktop-v0.11.0)"; then
  fail "desktop-v0.11.0 was accepted: $output"
elif [[ "$output" != *"::error::tag desktop-v0.11.0 does not name the workspace version 0.11.0; tag v0.11.0"* ]]; then
  fail "desktop-v0.11.0 failed without saying so: $output"
fi
if output="$(version_step 0.11.0 tag v0.11.1)"; then
  fail "v0.11.1 on a 0.11.0 commit was accepted: $output"
fi

# A stub `gh` on PATH: it logs each call to gh.log, answers `gh api` with
# GH_API_ANSWER, and finds no release, so "Publish the release" creates one.
stubs="$scratch/stubs"
mkdir -p "$stubs" "$scratch/runner"
cat > "$stubs/gh" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2" in
  "api "*) printf '%s\n' "$GH_API_ANSWER" ;;
  "release view") [[ "$*" == *--json* ]] && echo "https://github.com/x/y/releases/tag/$3" || exit 1 ;;
esac
SH
chmod +x "$stubs/gh"

# publish_step <version> <prerelease>: the gh calls "Publish the release"
# makes, in order, from a directory holding one asset and a stub
# release-notes.sh.
work="$scratch/publish"
mkdir -p "$work/assets" "$work/apps/desktop/scripts" "$scratch/runner/feed"
: > "$work/assets/SHA256SUMS"
printf '#!/usr/bin/env bash\necho notes\n' > "$work/apps/desktop/scripts/release-notes.sh"
chmod +x "$work/apps/desktop/scripts/release-notes.sh"
publish_step() {
  : > "$scratch/gh.log"
  (cd "$work" && PATH="$stubs:$PATH" STUB_LOG="$scratch/gh.log" RUNNER_TEMP="$scratch/runner" \
    VERSION="$1" TAG="v$1" PRERELEASE="$2" bash -e "$scratch/publish-step.sh" >"$scratch/error" 2>&1) \
    || { cat "$scratch/error"; return 1; }
  cat "$scratch/gh.log"
}
notes="--notes-file $scratch/runner/notes.md"
expected="release view v0.11.0-rc.1
release create v0.11.0-rc.1 --draft --prerelease --latest=false --verify-tag --title Steno 0.11.0-rc.1 $notes
release upload v0.11.0-rc.1 assets/SHA256SUMS --clobber
release edit v0.11.0-rc.1 --draft=false --prerelease --latest=false $notes
release view v0.11.0-rc.1 --json url --jq .url"
calls="$(publish_step 0.11.0-rc.1 true)" || fail "a candidate's publish failed: $calls"
[[ "$calls" == "$expected" ]] || fail "a candidate is published with these calls, not as a pre-release off latest:
$calls"
# A stable release: appcast.xml uploaded before it goes public and latest.
expected="release view v0.11.0
release create v0.11.0 --draft --prerelease --latest=false --verify-tag --title Steno 0.11.0 $notes
release upload v0.11.0 assets/SHA256SUMS --clobber
release upload v0.11.0 $scratch/runner/feed/appcast.xml --clobber
release edit v0.11.0 --draft=false --prerelease=false --latest $notes
release view v0.11.0 --json url --jq .url"
calls="$(publish_step 0.11.0 false)" || fail "a stable publish failed: $calls"
[[ "$calls" == "$expected" ]] || fail "a stable release is published with these calls, not public and latest after its appcast.xml:
$calls"

# The handoff job's first step passes only for an environment with a
# required reviewer, as the API's jq answers it.
reviewer_step() {
  : > "$scratch/gh.log"
  (PATH="$stubs:$PATH" STUB_LOG="$scratch/gh.log" GH_API_ANSWER="$1" GITHUB_REPOSITORY=NicolaiSchmid/steno \
    bash -e "$scratch/reviewer-step.sh" >"$scratch/error" 2>&1)
}
reviewer_step true || fail "an environment with a required reviewer was refused: $(cat "$scratch/error")"
# shellcheck disable=SC2016 # the jq filter is literal
[[ "$(cat "$scratch/gh.log")" == 'api repos/NicolaiSchmid/steno/environments/appcast --jq [.protection_rules[].type] | index("required_reviewers") != null' ]] \
  || fail "the environment check asks: $(cat "$scratch/gh.log")"
for answer in false ''; do
  if reviewer_step "$answer"; then
    fail "an environment without a required reviewer (gh answered '$answer') was accepted"
  elif ! grep -q '::error::the appcast environment has no required reviewer' "$scratch/error"; then
    fail "the environment check failed without saying so: $(cat "$scratch/error")"
  fi
done

# "Date the item" over a checkout whose origin's `appcast` branch holds the
# items given; the item it dates is build 3500 for 0.11.1.
origin="$scratch/origin"
checkout="$scratch/checkout"
git init --quiet --initial-branch=appcast "$origin"
git init --quiet "$checkout"
git -C "$checkout" remote add origin "$origin"
mkdir -p "$checkout/apps/desktop/scripts" "$checkout/sparkle-item"
cp "$here/handoff-appcast.py" "$checkout/apps/desktop/scripts/"
undated='Fri, 02 Oct 2026 10:57:59 +0200'
# feed_item <build> [<channel>]
feed_item() {
  printf '<item><title>%s</title><pubDate>%s</pubDate><sparkle:version>%s</sparkle:version>%s<enclosure url="https://example.com/%s.dmg" length="1" sparkle:edSignature="c2ln" /></item>\n' \
    "$1" "$undated" "$1" "${2:+<sparkle:channel>$2</sparkle:channel>}" "$1"
}
feed() {
  printf '<?xml version="1.0" encoding="utf-8"?>\n<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0"><channel><title>Steno</title>\n'
  printf '%s' "$@"
  printf '</channel></rss>\n'
}
# date_step <replace> <branch items...>: the step's outputs, or its error.
date_step() {
  local replace="$1"
  shift
  feed "$@" > "$origin/appcast.xml"
  git -C "$origin" add appcast.xml
  git -C "$origin" -c user.name=test -c user.email=test@example.com commit --quiet --allow-empty -m feed
  feed "$(feed_item 3500)" > "$checkout/sparkle-item/appcast.xml"
  : > "$scratch/output"
  (cd "$checkout" && GITHUB_OUTPUT="$scratch/output" RUNNER_TEMP="$scratch/runner" BUILD=3500 VERSION=0.11.1 \
    HANDOFF_ITEM_REPLACE="$replace" bash -e "$scratch/date-step.sh" >"$scratch/error" 2>&1) || { cat "$scratch/error"; return 1; }
  cat "$scratch/output"
}
dated() { ! grep -q "<pubDate>$undated</pubDate>" "$checkout/sparkle-item/appcast.xml"; }
betas="$(feed_item 3423 beta)$(feed_item 3424 beta)"

outputs="$(date_step '' "$betas")" || fail "the first approval was refused: $outputs"
[[ "$outputs" == 'on_branch=false' ]] || fail "the first approval outputs '$outputs'"
{ dated && grep -q '<pubDate>[A-Z][a-z][a-z], [0-9][0-9] [A-Z][a-z][a-z] 20[0-9][0-9] [0-9:]* +0000</pubDate>' "$checkout/sparkle-item/appcast.xml"; } \
  || fail "the first approval did not date the item: $(cat "$checkout/sparkle-item/appcast.xml")"

# A re-run after the item reached the branch keeps its date, so the
# phased rollout does not start again.
outputs="$(date_step '' "$betas" "$(feed_item 3500)")" || fail "a re-run was refused: $outputs"
[[ "$outputs" == 'on_branch=true' ]] || fail "a re-run outputs '$outputs', so the item would go on the branch again"
dated && fail "a re-run dated the item again"

# Another release's approval since publish looked: its handoff item, or a
# higher build.
for replace in '' 0.11.0; do
  if output="$(date_step "$replace" "$betas" "$(feed_item 3450)")"; then
    fail "a second handoff item was accepted with HANDOFF_ITEM_REPLACE='$replace': $output"
  elif [[ "$output" != *'::error::the appcast branch has a handoff item since publish looked'* ]]; then
    fail "a second handoff item failed without saying so: $output"
  fi
  dated && fail "the item was dated over another handoff item"
done
outputs="$(date_step 0.11.1 "$betas" "$(feed_item 3450)")" || fail "HANDOFF_ITEM_REPLACE=0.11.1 was refused: $outputs"
{ [[ "$outputs" == 'on_branch=false' ]] && dated; } || fail "HANDOFF_ITEM_REPLACE=0.11.1 did not date the item: $outputs"
if output="$(date_step 0.11.1 "$betas" "$(feed_item 3600 beta)")"; then
  fail "an item below the branch's highest build was accepted: $output"
elif [[ "$output" != *'build 3500 is not above build 3600'* ]]; then
  fail "an item below the branch's highest build failed without saying so: $output"
fi

if ((failures > 0)); then
  echo "release-workflow: $failures failed"
  exit 1
fi
echo "release-workflow: ok"
