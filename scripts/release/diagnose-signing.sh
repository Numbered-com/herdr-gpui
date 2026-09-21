#!/usr/bin/env bash
# TEMPORARY diagnostic. Reproduces the sign-macos.sh keychain setup on a runner
# without requiring a build, so signing failures can be iterated on quickly.
# Delete this file and .github/workflows/signing-diagnostics.yml once signing works.
#
# Never runs under `set -x`: it handles the Developer ID certificate.
# Deliberately does NOT use `set -e`; every step reports and execution continues
# so one run shows the full picture rather than stopping at the first failure.
set +x
set -uo pipefail

step=0
report() {
    local status=$1 desc=$2 err=${3:-}
    step=$((step + 1))
    if [[ $status == 0 ]]; then
        printf '%2d. OK    %s\n' "$step" "$desc"
    else
        printf '%2d. FAIL(%d) %s\n' "$step" "$status" "$desc"
        [[ -n $err ]] && printf '      stderr: %s\n' "$err"
    fi
}

echo "=============== environment ==============="
sw_vers
echo "uname: $(uname -a)"
echo "security: $(command -v security)"
echo "codesign: $(command -v codesign)"
echo "bash: $BASH_VERSION"
echo
echo "=============== inputs (no secret values) ==============="
for name in MACOS_CERTIFICATE_P12_BASE64 MACOS_CERTIFICATE_PASSWORD MACOS_SIGNING_IDENTITY; do
    value=${!name:-}
    printf '%s: present=%s length=%d\n' "$name" "$([[ -n $value ]] && echo yes || echo NO)" "${#value}"
done
printf 'MACOS_SIGNING_IDENTITY value: %s\n' "${MACOS_SIGNING_IDENTITY:-<unset>}"
echo

tmp=$(mktemp -d)
keychain=$tmp/signing.keychain-db
cleanup() { security delete-keychain "$keychain" >/dev/null 2>&1; rm -rf -- "$tmp"; }
trap cleanup EXIT

echo "=============== keychain search list as found ==============="
security list-keychains -d user
original_keychains=()
while IFS= read -r line; do
    [[ -z $line ]] && continue
    if [[ $line =~ ^[[:space:]]*\"(.*)\"[[:space:]]*$ ]]; then
        original_keychains+=("${BASH_REMATCH[1]}")
    else
        echo "UNPARSEABLE search list line: [$line]"
    fi
done <<< "$(security list-keychains -d user)"
printf 'parsed %d existing keychain(s)\n' "${#original_keychains[@]}"
echo

echo "=============== decode the certificate ==============="
printf '%s' "${MACOS_CERTIFICATE_P12_BASE64:-}" | base64 -D > "$tmp/certificate.p12" 2>"$tmp/e"
report $? "base64 -D of MACOS_CERTIFICATE_P12_BASE64" "$(cat "$tmp/e")"
printf 'decoded p12 size: %s bytes\n' "$(wc -c < "$tmp/certificate.p12" | tr -d ' ')"
echo

echo "=============== replicate sign-macos.sh:56-62 ==============="
password=$(openssl rand -hex 32)
security create-keychain -p "$password" "$keychain" 2>"$tmp/e"; report $? "create-keychain" "$(cat "$tmp/e")"
if [[ ${#original_keychains[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${original_keychains[@]}" 2>"$tmp/e"
    report $? "restore original search list (as line 57)" "$(cat "$tmp/e")"
else
    echo "    SKIPPED restoring search list: none were parsed"
fi
security set-keychain-settings -lut 21600 "$keychain" 2>"$tmp/e"; report $? "set-keychain-settings" "$(cat "$tmp/e")"
security unlock-keychain -p "$password" "$keychain" 2>"$tmp/e"; report $? "unlock-keychain" "$(cat "$tmp/e")"
security import "$tmp/certificate.p12" -k "$keychain" -P "${MACOS_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign >/dev/null 2>"$tmp/e"
report $? "import certificate (line 60)" "$(cat "$tmp/e")"
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null 2>"$tmp/e"
report $? "set-key-partition-list (line 62)" "$(cat "$tmp/e")"
echo

echo "=============== what actually landed in the keychain ==============="
echo "--- certificates ---"
security find-certificate -a "$keychain" 2>/dev/null | grep '"labl"' || echo "  (none)"
echo "--- identities valid for codesigning ---"
security find-identity -v -p codesigning "$keychain" 2>&1
echo "--- all identities, including invalid ---"
security find-identity -p codesigning "$keychain" 2>&1
echo

echo "=============== codesign a stub (line 75 equivalent) ==============="
echo "--- CASE A: signing keychain NOT in the search list (current behaviour) ---"
security list-keychains -d user
cp /bin/echo "$tmp/stubA"
codesign --force --sign "${MACOS_SIGNING_IDENTITY:-}" --keychain "$keychain" \
    --options runtime --timestamp "$tmp/stubA" 2>"$tmp/e"
report $? "CASE A codesign with --keychain only" "$(cat "$tmp/e")"

echo "--- CASE B: signing keychain ADDED to the search list (proposed fix) ---"
if [[ ${#original_keychains[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${original_keychains[@]}" "$keychain"
else
    security list-keychains -d user -s "$keychain"
fi
security list-keychains -d user
cp /bin/echo "$tmp/stubB"
codesign --force --sign "${MACOS_SIGNING_IDENTITY:-}" --keychain "$keychain" \
    --options runtime --timestamp "$tmp/stubB" 2>"$tmp/e"
report $? "CASE B codesign with keychain in search list" "$(cat "$tmp/e")"
codesign --verify --strict --verbose=2 "$tmp/stubB" 2>"$tmp/e"
report $? "CASE B codesign --verify" "$(cat "$tmp/e")"

# Leave the runner's search list as we found it.
if [[ ${#original_keychains[@]} -gt 0 ]]; then
    security list-keychains -d user -s "${original_keychains[@]}"
fi
echo
echo "=============== done ==============="
