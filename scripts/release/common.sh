#!/usr/bin/env bash
set -euo pipefail

# Shared with scripts sourcing this file; standalone ShellCheck cannot see them.
# shellcheck disable=SC2034
release_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
fail() { printf '%s\n' "$*" >&2; exit 1; }
version_check() {
    [[ $1 =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || fail 'Version must be numeric SemVer X.Y.Z (no v prefix or leading zeros)'
}
new_output() {
    [[ ! -e $1 && ! -L $1 ]] || fail "Output already exists: $1"
    [[ -d $(dirname -- "$1") ]] || fail "Output parent must exist: $1"
}
