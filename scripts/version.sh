#!/usr/bin/env bash
# Keep every copy of the release version in step with Cargo.toml.
#
#   scripts/version.sh check        fail if any file disagrees (CI runs this)
#   scripts/version.sh bump X.Y.Z   rewrite the plain version strings
#
# `bump` leaves the changelog entries to you: Debian, RPM, AppStream, the
# "Changes in" section of docs/CAPABILITIES.md, and the baseline and release
# notes. `check` then names any that are still missing.
set -euo pipefail
cd "$(dirname "$0")/.."

current() {
  sed -n 's/^version = "\([0-9][0-9.]*\)"\r\{0,1\}$/\1/p' Cargo.toml | head -n 1
}

# file|extended regex that must match, with @V@ standing for the version.
entries() {
  cat <<'LIST'
apps/rufust/ui/appwindow.slint|app-version: "@V@"
.github/workflows/release.yml|PACKAGE_VERSION: @V@
packaging/arch/PKGBUILD|^pkgver=@V@
packaging/rpm/rufust.spec|^Version: +@V@
packaging/windows/rufust.iss|/DVersion=@V@
docs/BUILDING.md|/DVersion=@V@
docs/BUILDING.md|rufust\.AppImage @V@ 
packaging/rpm/rufust.spec|^\* .* - @V@-1
packaging/debian/changelog|^rufust \(@V@-1\)
packaging/metainfo/io.github.nitrodiesel.rufust.metainfo.xml|<release version="@V@"
docs/CAPABILITIES.md|^## Changes in @V@
docs/PROJECT_CONTEXT.md|^## Current baseline: @V@
.github/workflows/release.yml|New in @V@:
LIST
}

check() {
  local version failed=0 file pattern
  version="$(current)"
  [ -n "$version" ] || { echo "no workspace version in Cargo.toml" >&2; exit 1; }
  while IFS='|' read -r file pattern; do
    pattern="${pattern//@V@/${version//./\.}}"
    if ! grep -Eq -- "$pattern" "$file"; then
      echo "$file: nothing matches '$pattern'" >&2
      failed=1
    fi
  done < <(entries)
  # The newest changelog entries must be this version, not just mention it.
  head -n 1 packaging/debian/changelog | grep -q "($version-1)" ||
    { echo "packaging/debian/changelog: newest entry is not $version-1" >&2; failed=1; }
  grep -m 1 '<release version=' packaging/metainfo/io.github.nitrodiesel.rufust.metainfo.xml |
    grep -q "\"$version\"" ||
    { echo "metainfo: newest release is not $version" >&2; failed=1; }
  [ "$failed" -eq 0 ] || exit 1
  echo "version $version is consistent"
}

bump() {
  local new="$1" old
  [[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "usage: $0 bump X.Y.Z" >&2; exit 2; }
  old="$(current)"
  local o="${old//./\.}"
  sed -i "s/^version = \"$o\"/version = \"$new\"/" Cargo.toml
  sed -i "s/app-version: \"$o\"/app-version: \"$new\"/" apps/rufust/ui/appwindow.slint
  sed -i "s/PACKAGE_VERSION: $o/PACKAGE_VERSION: $new/" .github/workflows/release.yml
  sed -i "s/^pkgver=$o/pkgver=$new/" packaging/arch/PKGBUILD
  sed -i "s/^\(Version: *\)$o/\1$new/" packaging/rpm/rufust.spec
  sed -i "s|/DVersion=$o|/DVersion=$new|" packaging/windows/rufust.iss docs/BUILDING.md
  sed -i "s/rufust\.AppImage $o /rufust.AppImage $new /" docs/BUILDING.md
  cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace
  echo "bumped $old -> $new; now write the changelog entries:"
  check || true
}

case "${1:-}" in
  check) check ;;
  bump) bump "${2:-}" ;;
  *) echo "usage: $0 check | bump X.Y.Z" >&2; exit 2 ;;
esac
