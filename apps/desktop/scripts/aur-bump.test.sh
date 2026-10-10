#!/usr/bin/env bash
# Checks apps/desktop/scripts/aur-bump.sh against a copy of packaging/aur
# and a local bare repository standing in for the AUR: the PKGBUILD and
# .SRCINFO lines it rewrites (and only those), the files it pushes, a
# second run, and every refusal. The copy is the package as it will be
# after the first push by hand (Bump step 3 done, so no drop-in copies);
# the checked-in package, which still has them, is refused. rust-ci.yml
# runs it on Linux.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/aur-bump.sh"
package="$here/../../../packaging/aur"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}
git_quiet() { git -c init.defaultBranch=master -c user.name=test -c user.email=test@example.com "$@"; }

# A repository root holding packaging/aur as given.
root() {
  mkdir -p "$scratch/$1/packaging"
  cp -R "$package" "$scratch/$1/packaging/aur"
  echo "$scratch/$1"
}

# Bump step 3 on a copy: the drop-in copies, their sources, checksums and
# _copy calls go.
stepped="$(root stepped)"
aur="$stepped/packaging/aur"
copies=(autostart-service-stop-timeout.conf gnome-scope-stop-timeout.conf)
for copy in "${copies[@]}"; do
  index="$(sed -n 's/^\tsource = //p' "$aur/.SRCINFO" | grep -nxF "$copy" | cut -d: -f1)"
  sum="$(sed -n 's/^\tsha256sums = //p' "$aur/.SRCINFO" | sed -n "${index}p")"
  sed -i "/^  '$copy'$/d; /'$sum'/d; /^  _copy $copy /d" "$aur/PKGBUILD"
  sed -i "/^\tsource = $copy$/d; /^\tsha256sums = $sum$/d" "$aur/.SRCINFO"
  rm "$aur/$copy"
done
cp -R "$aur" "$scratch/before"

# The AUR, holding the pinned package and a file no longer in it.
git_quiet init --quiet --bare "$scratch/aur.git"
git_quiet init --quiet "$scratch/aur-seed"
cp -R "$package/." "$scratch/aur-seed/"
rm "$scratch/aur-seed/README.md"
git_quiet -C "$scratch/aur-seed" add -A
git_quiet -C "$scratch/aur-seed" commit --quiet -m 'steno-desktop-bin 0.1.0rc3-1'
git_quiet -C "$scratch/aur-seed" push --quiet "$scratch/aur.git" HEAD:master

mkdir -p "$scratch/assets"
printf 'deb bytes\n' > "$scratch/assets/steno-desktop_0.11.1_amd64.deb"
printf 'asc bytes\n' > "$scratch/assets/steno-desktop_0.11.1_amd64.deb.asc"
deb_sha="$(sha256sum "$scratch/assets/steno-desktop_0.11.1_amd64.deb" | cut -d' ' -f1)"
asc_sha="$(sha256sum "$scratch/assets/steno-desktop_0.11.1_amd64.deb.asc" | cut -d' ' -f1)"

# run <root> <version> [<assets>]: the script with the stand-in AUR.
run() {
  STENO_REPO_ROOT="$1" AUR_URL="$scratch/aur.git" "$script" "$2" "${3:-$scratch/assets}" 2>&1
}
refused() {
  local why="$1" expected="$2" output
  shift 2
  if output="$("$@" 2>&1)"; then
    fail "$why passed: $output"
  elif [[ "$output" != *"::error::"*"$expected"* ]]; then
    fail "$why failed without \"$expected\": $output"
  fi
}

if output="$(run "$stepped" 0.11.1)"; then
  [[ "$output" == *"pushed steno-desktop-bin 0.11.1-1 to the AUR"* ]] || fail "nothing was pushed: $output"
else
  fail "the bump failed: $output"
fi

# Exactly the lines a bump changes.
release=https://github.com/NicolaiSchmid/steno/releases/download/v0.11.1
expected_pkgbuild="$(cat <<EOF
-pkgver=0.1.0rc3
+pkgver=0.11.1
-sha256sums=('2f2dfb3b3452a5473e7de6ab44a3733cf143871cf7da14d708a78ffc532e60e5'
-            '8d4bc8496033be5835365d35adcfd60dea5521813ea347550216a6af3cc34007'
+sha256sums=('$deb_sha'
+            '$asc_sha'
EOF
)"
got="$({ diff "$scratch/before/PKGBUILD" "$aur/PKGBUILD" || true; } | grep '^[<>]' | sed 's/^< /-/; s/^> /+/' | sort)"
[[ "$got" == "$(sort <<< "$expected_pkgbuild")" ]] || fail "the PKGBUILD changed otherwise: $got"
expected_srcinfo="$(cat <<EOF
-	pkgver = 0.1.0rc3
+	pkgver = 0.11.1
-	provides = steno-desktop=0.1.0rc3
+	provides = steno-desktop=0.11.1
-	source = https://github.com/NicolaiSchmid/steno/releases/download/desktop-v0.1.0-rc.3/steno-desktop_0.1.0-rc.3_amd64.deb
+	source = $release/steno-desktop_0.11.1_amd64.deb
-	source = https://github.com/NicolaiSchmid/steno/releases/download/desktop-v0.1.0-rc.3/steno-desktop_0.1.0-rc.3_amd64.deb.asc
+	source = $release/steno-desktop_0.11.1_amd64.deb.asc
-	sha256sums = 2f2dfb3b3452a5473e7de6ab44a3733cf143871cf7da14d708a78ffc532e60e5
+	sha256sums = $deb_sha
-	sha256sums = 8d4bc8496033be5835365d35adcfd60dea5521813ea347550216a6af3cc34007
+	sha256sums = $asc_sha
EOF
)"
got="$({ diff "$scratch/before/.SRCINFO" "$aur/.SRCINFO" || true; } | grep '^[<>]' | sed 's/^< /-/; s/^> /+/' | sort)"
[[ "$got" == "$(sort <<< "$expected_srcinfo")" ]] || fail ".SRCINFO changed otherwise: $got"
# The PKGBUILD's own URLs follow from pkgver (v<version> from 0.11.0 on).
grep -qF "*) _tag=v\$_version ;;" "$aur/PKGBUILD" || fail "the PKGBUILD no longer derives v<version> tags"

# What the AUR holds now: the package's files flat, nothing else.
git_quiet clone --quiet "$scratch/aur.git" "$scratch/aur-after"
[[ "$(git -C "$scratch/aur-after" log -1 --format=%s)" == 'steno-desktop-bin 0.11.1-1' ]] \
  || fail "the AUR commit is '$(git -C "$scratch/aur-after" log -1 --format=%s)'"
files="$(git -C "$scratch/aur-after" ls-files | LC_ALL=C sort | tr '\n' ' ')"
[[ "$files" == ".SRCINFO .gitignore LICENSE PKGBUILD keys/pgp/048B527950E4F609B90E63495F8810A6E6D4DB46.asc speexdsp-COPYING steno-desktop.sh " ]] \
  || fail "the AUR holds $files"
cmp -s "$aur/PKGBUILD" "$scratch/aur-after/PKGBUILD" || fail "the AUR's PKGBUILD is not the bumped one"

# A second run changes nothing and pushes nothing.
output="$(run "$stepped" 0.11.1)" || fail "the second run failed: $output"
[[ "$output" == *"the AUR already has steno-desktop-bin 0.11.1-1"* ]] || fail "the second run did not see the AUR current: $output"
[[ "$(git -C "$scratch/aur.git" rev-list --count master)" == 2 ]] || fail "the second run pushed"

refused 'a candidate' 'the AUR takes stable releases only, not 0.11.1-rc.1' run "$stepped" 0.11.1-rc.1
printf 'deb bytes\n' > "$scratch/assets/steno-desktop_0.11.2_amd64.deb"
refused 'a missing .asc' 'has no steno-desktop_0.11.2_amd64.deb.asc' run "$stepped" 0.11.2
refused 'the package with its drop-in copies' 'still installs drop-in copies' run "$(root pinned)" 0.11.1
remote="$(root remote)"
sed -i 's|^\tsource = LICENSE$|\tsource = https://example.com/LICENSE|' "$remote/packaging/aur/.SRCINFO"
refused 'a third remote source' 'nothing else remote' run "$remote" 0.11.1
refused 'no key and no AUR_URL' 'AUR_SSH_PRIVATE_KEY is not set' \
  env -u AUR_SSH_PRIVATE_KEY STENO_REPO_ROOT="$stepped" "$script" 0.11.1 "$scratch/assets"
cmp -s "$aur/PKGBUILD" "$scratch/aur-after/PKGBUILD" || fail "a refused run changed the PKGBUILD"

if ((failures > 0)); then
  echo "aur-bump: $failures failed"
  exit 1
fi
echo "aur-bump: ok"
