#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo "usage: $0 <version> <target> <binary> <output-dir>" >&2
  exit 1
fi
version="$1"
target="$2"
binary="$3"
output="$4"
[[ "$version" =~ ^[0-9]{8}\.[0-9]{2}$ ]] || exit 1
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;;
  *) echo "unsupported Linux target: $target" >&2; exit 1 ;;
esac

name="herdr-gpui-${version}-${target}"
mkdir -p "$output"
staging="$(mktemp -d "$output/.linux-package.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
mkdir -p "$staging/$name/bin"
install -m 0755 "$binary" "$staging/$name/bin/herdr-gpui"
cp README.md LICENSE NOTICE "$staging/$name/"
tar -C "$staging" -czf "$output/$name.tar.gz" "$name"
