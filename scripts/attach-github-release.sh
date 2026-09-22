#!/usr/bin/env bash
# Upload verified desktop packages to the GitHub Release for a version tag.
# Creates the Release when the tag was pushed without one. Existing release
# notes are left unchanged; assets with the same name are replaced.

set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: attach-github-release.sh <tag> <asset>..." >&2
  exit 1
fi

tag="$1"
shift

if [[ -z "${GH_TOKEN:-}" ]]; then
  echo "GH_TOKEN is required." >&2
  exit 1
fi

for asset in "$@"; do
  if [[ ! -f "$asset" ]]; then
    echo "Release asset does not exist: $asset" >&2
    exit 1
  fi
done

root="$(cd "$(dirname "$0")/.." && pwd)"
python3 "$root/scripts/check-desktop-release-tag.py" --root "$root" --tag "$tag"

if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" \
    --verify-tag \
    --title "$tag" \
    --notes "Desktop installers for ${tag}." \
    || gh release view "$tag" >/dev/null
fi

gh release upload "$tag" "$@" --clobber
