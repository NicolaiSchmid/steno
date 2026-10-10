#!/usr/bin/env bash
# Checks .github/workflows/desktop-release.yml against what the stable
# plan's "Release mechanics" requires of it (S7 of
# .plans/2026-10-07-stable-promotion.md), the assertions of the Swift
# release workflow's tests among them: `v*` tags; the build number in the
# Mac bundle and checked against the `appcast` branch; the handoff item
# signed on tags only, by the one step that sees SPARKLE_PRIVATE_KEY, into
# its own artifact; `publish` makes a stable release public and "latest"
# with appcast.xml before the cask bump, and a pre-release bumps nothing;
# `handoff` needs `publish` behind the `appcast` environment; `contents:
# write` where a job writes. It also runs `plan`'s version step, as the
# workflow has it, over a scratch repository: the tag it accepts, the
# pre-release flag the stable-only steps read, the build number.
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
check(jobs["handoff"]["permissions"] == {"contents": "write"}, "handoff's permissions are %r" % jobs["handoff"].get("permissions"))

# plan: the whole history, the outputs, the build against the branch.
check(checkout("plan").get("with", {}).get("fetch-depth") == 0, "plan does not check out the whole history")
for output in ("version", "prerelease", "build"):
    check(output in jobs["plan"]["outputs"], "plan does not output %s" % output)
check(index("plan", "Build number against the appcast branch") > 0, "plan does not check the build number")
build_check = step("plan", "Build number against the appcast branch").get("run", "")
check("handoff-appcast.py check-build" in build_check, "plan's build check does not run check-build")

# bundle: the whole history on macOS, the build number in CFBundleVersion
# through the configuration file Bundle reads too.
depth = checkout("bundle").get("with", {}).get("fetch-depth")
check(depth == "${{ matrix.name == 'macos' && '0' || '1' }}", "the bundle checkout's fetch-depth is %r" % depth)
build = step("bundle", "Build").get("run", "")
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
release = step("publish", "Publish the release").get("run", "")
check('--title "Steno $VERSION"' in release, "the release title is not 'Steno <version>'")
public = release.find("--draft=false --prerelease=false --latest ")
uploaded = release.find('gh release upload "$TAG" "$RUNNER_TEMP/feed/appcast.xml"')
check(public > 0, "a stable release is not made public and latest")
check(0 < uploaded < public, "appcast.xml is not uploaded before the release is made public")
check("--draft=false --prerelease --latest=false" in release, "a pre-release is not kept off latest")
order = [index("publish", name) for name in ("Appcast from the branch", "Publish the release", "Update lanes", "Bump the Homebrew cask")]
check(order == sorted(order) and -1 not in order, "publish's steps are out of order: %r" % order)
for name in ("Bump the Homebrew cask", "Push the AUR bump", "Nix flake lines in the summary"):
    check(step("publish", name).get("if") == stable, "%r runs for a pre-release: %r" % (name, step("publish", name).get("if")))
check("bump-homebrew-cask.sh" in step("publish", "Bump the Homebrew cask").get("run", ""), "the cask step does not bump the cask")
check("aur-bump.sh" in step("publish", "Push the AUR bump").get("run", ""), "the AUR step does not run aur-bump.sh")
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
dated = index("handoff", "Date the item")
pushed = index("handoff", "Put the item on the appcast branch")
asset = index("handoff", "The branch's feed as the release's appcast.xml")
check(0 < dated < pushed < asset, "handoff's steps are out of order: %r" % names)
dating = step("handoff", "Date the item")
check("stamp-pubdate" in dating.get("run", ""), "the item is not dated")
check("has-build" in dating.get("run", "") and dating.get("id") == "date", "a re-run would date the item on the branch again")
push = step("handoff", "Put the item on the appcast branch")
check(push.get("if") == "steps.date.outputs.on_branch == 'false'", "the item goes on the branch again on a re-run: %r" % push.get("if"))
check(push.get("run") == 'apps/macos/scripts/publish-appcast.sh "$TAG" false', "the item goes on with %r" % push.get("run"))
check(push.get("env", {}).get("STENO_RELEASE_APPCAST", "").endswith("/sparkle-item/appcast.xml"), "publish-appcast.sh does not read sparkle-item")
check("--clobber" in step("handoff", "The branch's feed as the release's appcast.xml").get("run", ""), "the release's appcast.xml is not replaced")

# plan's version step, run as written over a scratch repository.
with open(scratch + "/version-step.sh", "w", encoding="utf-8") as handle:
    handle.write(next(s for s in steps("plan") if s.get("id") == "version")["run"])

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

if ((failures > 0)); then
  echo "release-workflow: $failures failed"
  exit 1
fi
echo "release-workflow: ok"
