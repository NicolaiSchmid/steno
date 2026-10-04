#!/usr/bin/env bash
# Which updater lanes a release moves. Each lane only moves forward: the
# stable lane (`desktop-stable`) takes a release (no hyphen), the beta lane
# (`desktop-beta`) every version, each only when the version is at or above
# the one the lane serves. An empty current version means the lane does not
# exist yet, so it moves. The same version moves again, so a rerun of a tag
# is idempotent; an older tag or a hotfix on an older line (0.2.2 after
# 0.3.0) leaves a lane where it is.
#
# Versions are SemVer 2.0 and compared by its precedence: the cores
# numerically, a release above its pre-releases, pre-release identifiers
# one by one (numbers numerically and below words, words in ASCII order,
# a shorter list below a longer one it starts), build metadata ignored.
# Prints the lanes to move, one per line, and on stderr why a lane stays.
# A version that is not SemVer (a number with a leading zero included) is
# an `::error::` and exit 1.
#
#   apps/desktop/scripts/updater-lanes.sh <version> <beta's version> <stable's version>
#
# apps/desktop/scripts/updater-lanes.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail
export LC_ALL=C

version="${1:?version, e.g. 0.2.0-rc.1}"
beta="${2-}"
stable="${3-}"

number='(0|[1-9][0-9]*)'
# A pre-release identifier is a number without a leading zero or a word;
# build metadata takes any.
identifier="($number|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
build='[0-9A-Za-z-]+'
semver="^$number\.$number\.$number(-$identifier(\.$identifier)*)?(\+$build(\.$build)*)?$"

check() {
  [[ "$1" =~ $semver ]] || { echo "::error::$2 \"$1\" is not a SemVer version" >&2; exit 1; }
}

# compare_numbers <a> <b>: -1, 0 or 1, by length first, so none overflows.
compare_numbers() {
  if ((${#1} != ${#2})); then
    ((${#1} < ${#2})) && echo -1 || echo 1
  elif [[ "$1" == "$2" ]]; then
    echo 0
  else
    [[ "$1" < "$2" ]] && echo -1 || echo 1
  fi
}

# compare <a> <b>: -1, 0 or 1 by SemVer precedence.
compare() {
  local a="${1%%+*}" b="${2%%+*}" core_a core_b pre_a="" pre_b="" i result
  core_a="${a%%-*}" core_b="${b%%-*}"
  [[ "$a" == *-* ]] && pre_a="${a#*-}"
  [[ "$b" == *-* ]] && pre_b="${b#*-}"
  local -a xs ys
  IFS=. read -ra xs <<< "$core_a"
  IFS=. read -ra ys <<< "$core_b"
  for i in 0 1 2; do
    result="$(compare_numbers "${xs[i]}" "${ys[i]}")"
    [[ "$result" == 0 ]] || { echo "$result"; return; }
  done
  # A release outranks its own pre-releases.
  if [[ -z "$pre_a" || -z "$pre_b" ]]; then
    if [[ -z "$pre_a" && -z "$pre_b" ]]; then echo 0
    elif [[ -z "$pre_a" ]]; then echo 1
    else echo -1
    fi
    return
  fi
  IFS=. read -ra xs <<< "$pre_a"
  IFS=. read -ra ys <<< "$pre_b"
  for ((i = 0; i < ${#xs[@]} && i < ${#ys[@]}; i++)); do
    local x="${xs[i]}" y="${ys[i]}" x_number=false y_number=false
    [[ "$x" =~ ^[0-9]+$ ]] && x_number=true
    [[ "$y" =~ ^[0-9]+$ ]] && y_number=true
    if [[ "$x_number" == true && "$y_number" == true ]]; then
      result="$(compare_numbers "$x" "$y")"
    elif [[ "$x_number" == true ]]; then
      result=-1
    elif [[ "$y_number" == true ]]; then
      result=1
    elif [[ "$x" == "$y" ]]; then
      result=0
    elif [[ "$x" < "$y" ]]; then
      result=-1
    else
      result=1
    fi
    [[ "$result" == 0 ]] || { echo "$result"; return; }
  done
  compare_numbers "${#xs[@]}" "${#ys[@]}"
}

check "$version" version
[[ -z "$beta" ]] || check "$beta" "the beta lane's version"
[[ -z "$stable" ]] || check "$stable" "the stable lane's version"

# lane <name> <current>: moves unless it serves a later version.
lane() {
  if [[ -z "$2" || "$(compare "$version" "$2")" != -1 ]]; then
    echo "desktop-$1"
  else
    echo "the $1 lane keeps $2, a later version than $version" >&2
  fi
}

if [[ "${version%%+*}" != *-* ]]; then
  lane stable "$stable"
fi
lane beta "$beta"
