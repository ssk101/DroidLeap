#!/usr/bin/env bash
# Bumps the version everywhere, commits it, tags it vX.Y.Z and pushes both.
# The tag push starts the release build, which checks that the tag matches these files.
#
#   ./release.sh patch|minor|major|X.Y.Z [-y]
#
# The current version is the one in desktop/Cargo.toml. -y skips the confirmation.
set -euo pipefail

cd "$(dirname "$0")"

CARGO_TOML=desktop/Cargo.toml
CARGO_LOCK=desktop/Cargo.lock
GRADLE=android/app/build.gradle.kts
FILES=("$CARGO_TOML" "$CARGO_LOCK" "$GRADLE")
BRANCH=master

die() { echo "release: $*" >&2; exit 1; }

[[ $# -ge 1 && $# -le 2 ]] || die "usage: $0 patch|minor|major|X.Y.Z [-y]"
bump=$1
assume_yes=false
if [[ ${2:-} == -y ]]; then assume_yes=true; elif [[ $# -eq 2 ]]; then die "unknown option $2"; fi

current=$(sed -n 's/^version = "\(.*\)"/\1/p' "$CARGO_TOML" | head -1)
[[ $current =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || die "cannot read the version from $CARGO_TOML"
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}

case $bump in
  patch) new="$major.$minor.$((patch + 1))" ;;
  minor) new="$major.$((minor + 1)).0" ;;
  major) new="$((major + 1)).0.0" ;;
  *)
    [[ $bump =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "'$bump' is not patch, minor, major or X.Y.Z"
    new=$bump
    ;;
esac
tag="v$new"

code=$(sed -n 's/^ *versionCode = \([0-9]*\)$/\1/p' "$GRADLE")
[[ -n $code ]] || die "cannot read versionCode from $GRADLE"
new_code=$((code + 1))

[[ $(git branch --show-current) == "$BRANCH" ]] || die "not on $BRANCH"
git diff --cached --quiet || die "there are staged changes; commit or unstage them first"
git diff --quiet -- "${FILES[@]}" || die "${FILES[*]} have uncommitted changes"
git fetch --quiet origin "$BRANCH" --tags
[[ -z $(git rev-list "HEAD..origin/$BRANCH") ]] || die "$BRANCH is behind origin; pull first"
git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists"

echo "version  $current -> $new (Android versionCode $code -> $new_code)"
echo "commit   'Bump version to $new' on $BRANCH, tag $tag, push both to origin"
if ! $assume_yes; then
  read -r -p "Continue? [y/N] " answer
  [[ $answer == [yY] ]] || die "cancelled"
fi

sed -i "0,/^version = \".*\"/s//version = \"$new\"/" "$CARGO_TOML"
sed -i "/^name = \"droidleap\"\$/{n;s/^version = .*/version = \"$new\"/}" "$CARGO_LOCK"
sed -i \
  -e "s/^\( *versionCode = \).*/\1$new_code/" \
  -e "s/^\( *versionName = \).*/\1\"$new\"/" \
  "$GRADLE"

# Same extraction as the release workflow, so a mismatch fails here instead of after the push.
check() { [[ $1 == "$new" ]] || { git checkout -- "${FILES[@]}"; die "$2 did not end up at $new (got '$1'); changes reverted"; }; }
check "$(sed -n 's/^version = "\(.*\)"/\1/p' "$CARGO_TOML" | head -1)" "$CARGO_TOML"
check "$(sed -n '/^name = "droidleap"$/{n;s/^version = "\(.*\)"/\1/p}' "$CARGO_LOCK")" "$CARGO_LOCK"
check "$(sed -n 's/^ *versionName = "\(.*\)"/\1/p' "$GRADLE")" "$GRADLE"

git add -- "${FILES[@]}"
git commit --quiet -m "Bump version to $new" -- "${FILES[@]}"
git tag "$tag"
git push --atomic origin "$BRANCH" "$tag"
echo "released $tag"
