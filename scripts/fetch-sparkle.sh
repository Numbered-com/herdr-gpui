#!/usr/bin/env bash
# Fetch the exact framework and signing tools used by release packaging.
set -euo pipefail

if [[ $# -ne 1 || -z "$1" ]]; then
  printf 'Usage: bash scripts/fetch-sparkle.sh DESTINATION\n' >&2
  exit 2
fi

destination="$1"
if [[ -e "$destination" || -L "$destination" ]]; then
  printf 'Destination must not already exist: %s\n' "$destination" >&2
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/herdr-sparkle.XXXXXX")"
trap 'rm -rf "$work"' EXIT
url='https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz'
sha256='c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c'

curl --fail --silent --show-error --location \
  --proto '=https' --proto-redir '=https' \
  --connect-timeout 15 --max-time 180 --max-filesize 104857600 \
  --output "$work/Sparkle.tar.xz" "$url"
actual="$(shasum -a 256 "$work/Sparkle.tar.xz")"
if [[ "${actual%% *}" != "$sha256" ]]; then
  printf 'Sparkle 2.10.0 SHA256 mismatch; refusing to extract or run tools\n' >&2
  exit 1
fi

# Extraction happens only after verification; preserve framework symlinks/modes.
mkdir "$work/distribution"
tar -xJf "$work/Sparkle.tar.xz" -C "$work/distribution"
test -f "$work/distribution/Sparkle.framework/Versions/B/Sparkle"
test -x "$work/distribution/bin/generate_appcast"
test -x "$work/distribution/bin/sign_update"
mv "$work/distribution" "$destination"
printf 'Verified Sparkle 2.10.0 extracted to %s\n' "$destination"
