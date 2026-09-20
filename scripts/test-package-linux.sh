#!/usr/bin/env bash
# Archive structure/permissions and argument validation, without building a GUI.
set -euo pipefail
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
  name="herdr-gpui-00000000.00-${target}"
  bash scripts/package-linux.sh 00000000.00 "$target" /usr/bin/true "$temporary/output dir"
  tar -xzf "$temporary/output dir/$name.tar.gz" -C "$temporary"
  test -x "$temporary/$name/bin/herdr-gpui"
  cmp /usr/bin/true "$temporary/$name/bin/herdr-gpui"
  for file in README.md LICENSE NOTICE; do
    cmp "$file" "$temporary/$name/$file"
  done
done
if bash scripts/package-linux.sh bad-version x86_64-unknown-linux-gnu /usr/bin/true "$temporary"; then
  exit 1
fi
if bash scripts/package-linux.sh 00000000.00 x86_64-apple-darwin /usr/bin/true "$temporary"; then
  exit 1
fi
if bash scripts/package-linux.sh 00000000.00 x86_64-unknown-linux-gnu "$temporary/missing" "$temporary"; then
  exit 1
fi
echo "Linux packaging checks passed"
