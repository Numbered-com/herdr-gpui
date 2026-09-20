# Shared Rust Updater Releases

The updater authenticates a small JSON manifest using an Ed25519 public key
embedded in the executable. macOS updates replace `Herdr.app`; the Linux archive
contract replaces a standalone executable. Installation requires explicit user
approval. Updating the GUI must not stop or upgrade the daemon or its terminals.

The release pipeline currently **builds macOS only**, on Apple Silicon and Intel,
and combines both executables into a universal app. Linux archive packaging is
ready for a separate Linux builder, but no Linux artifacts are generated until
that builder is added. Package-managed Linux installs should use their package
manager rather than overwrite managed files.

## Repository Configuration

| Setting | Kind | Contents |
| --- | --- | --- |
| `HERDR_UPDATE_PUBLIC_KEY` | Actions variable | Exactly 64 lowercase hex characters encoding the 32-byte Ed25519 public key |
| `HERDR_UPDATE_SIGNING_KEY` | Actions secret | Unencrypted Ed25519 private key PEM, including header/footer and line breaks |

Both macOS architecture builds receive `HERDR_UPDATE_PUBLIC_KEY` and
`HERDR_RELEASE_VERSION` as compile-time environment variables. Future Linux builds
must embed the same values. The public key is required and format-validated even
for dry runs. The private key is loaded only in the real-release manifest signing
step, never in compilation or pull-request tests. Signing rejects a missing key,
a non-Ed25519 key, or a key whose DER public-key encoding does not exactly match
the configured public key.

Apple signing/notarization separately requires `APPLE_CERTIFICATE_BASE64`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_KEY_P8`, `APPLE_API_KEY_ID`, and
`APPLE_API_ISSUER_ID`. Sigstore uses GitHub OIDC. Protect release tags and workflow
changes: trusted release code can use these credentials.

### Initial Key Setup

Run deliberately on a trusted administrator machine, not in ordinary builds.
Use Homebrew's explicit OpenSSL 3 path on macOS; `/usr/bin/openssl` may be LibreSSL
and may not support Ed25519. Linux can use `/usr/bin/openssl` version 3.

```sh
set -euo pipefail
set +x
OPENSSL="$(brew --prefix openssl@3)/bin/openssl"
"$OPENSSL" version
umask 077
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"$OPENSSL" genpkey -algorithm ED25519 -out "$work/private.pem"
"$OPENSSL" pkey -in "$work/private.pem" -pubout -outform DER -out "$work/public.der"
public="$(python3 - "$work/public.der" <<'PY'
from pathlib import Path
import sys
der = Path(sys.argv[1]).read_bytes()
assert len(der) == 44 and der[:12].hex() == "302a300506032b6570032100"
print(der[12:].hex())
PY
)"
gh variable set HERDR_UPDATE_PUBLIC_KEY --repo penso/herdr-gpui --body "$public"
gh secret set HERDR_UPDATE_SIGNING_KEY --repo penso/herdr-gpui < "$work/private.pem"
# Securely back up the private key in encrypted storage before leaving this shell.
```

Never commit the key, log it, put it in command-line arguments, or enable shell
tracing while handling it. Release signing writes it to a private temporary
directory with a 0600 file, unsets the environment variable before invoking tools,
and removes the directory with an exit trap. Only the public key and signatures
are distributed. The workflow selects the runner's `openssl@3` formula explicitly
and checks major version 3; the Homebrew patch version follows the runner image,
not a separately checksum-pinned binary.

Keep an encrypted backup independent of CI. Existing installs trust their embedded
key; changing the repository variable is not a key-rotation protocol. Key loss or
compromise requires a reviewed recovery plan or manual reinstall, not disabling
verification.

## Authentication Contract

The source of truth is `crates/herdr-gpui/src/updater/release.rs`. Release assets
include `update-manifest.json` and `update-manifest.sig`. Example JSON, formatted
here only for readability:

```json
{
  "schema": 1,
  "version": "20260920.01",
  "assets": [
    {
      "target": "universal-apple-darwin",
      "name": "herdr-gpui-20260920.01-macos-universal.app.tar.gz",
      "size": 123456,
      "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    }
  ]
}
```

- Versions are real calendar dates in fixed-width `YYYYMMDD.NN` form.
- JSON has exactly these fields, schema 1, at most 65536 bytes, and one to three
  unique supported targets. The release generator always requires the current
  version's macOS archive and includes either Linux archive when present.
- Supported targets are `universal-apple-darwin`, `x86_64-unknown-linux-gnu`, and
  `aarch64-unknown-linux-gnu`. Linux names are `herdr-gpui-VERSION-TARGET.tar.gz`.
- Each compressed archive is 1 through 268435456 bytes, with its exact byte length
  and lowercase 64-character SHA-256 digest in the manifest.
- `update-manifest.sig` is a **raw 64-byte Ed25519 signature**, not hex, base64,
  PEM, or a Sigstore signature. OpenSSL signs the exact JSON bytes using
  `pkeyutl -sign -rawin`. Any whitespace change requires regenerating the signature.

The client discovers the latest stable release through
`https://api.github.com/repos/penso/herdr-gpui/releases/latest`. Manifest, signature,
and archive downloads use version-specific GitHub release URLs. The authenticated
version must match the release tag; archive names and sizes must also match the
release metadata. SHA-256 and length are checked before extraction.

The JSON signature is named `update-manifest.sig`. Sigstore sidecars remain
separate: `update-manifest.json.sig` signs the JSON through Sigstore, while
`update-manifest.sig.sig` signs the raw Ed25519 signature through Sigstore. The
updater's trust anchor is the embedded Ed25519 key, not these Sigstore sidecars.

## Packaging API

Python 3.9 or newer is required. All output paths must be new and have existing
parent directories; commands refuse to overwrite existing outputs.

```sh
python3 scripts/update-manifest.py package-macos dist/Herdr.app \
  dist/herdr-gpui-20260920.01-macos-universal.app.tar.gz
python3 scripts/update-manifest.py package-linux path/to/herdr-gpui \
  x86_64-unknown-linux-gnu 20260920.01 \
  dist/herdr-gpui-20260920.01-x86_64-unknown-linux-gnu.tar.gz
python3 scripts/update-manifest.py create dist 20260920.01
HERDR_UPDATE_PUBLIC_KEY="$public" python3 scripts/update-manifest.py validate-public-key
HERDR_UPDATE_PUBLIC_KEY="$public" python3 scripts/update-manifest.py check-key \
  "$OPENSSL" path/to/private.pem
```

`package-macos APP ARCHIVE` requires an actual directory named `Herdr.app`, and
stores all entries beneath that single root. `package-linux BINARY TARGET VERSION
DESTINATION` stores exactly one regular file named `herdr-gpui-VERSION-TARGET`,
mode 0755. It rejects symlink inputs.

Both commands use Python `tarfile` USTAR format inside gzip, with no PAX or GNU
extensions, hardlink records, devices, or FIFOs. macOS modes and relative symlinks
are preserved, but links escaping the bundle and special permission bits are
rejected. Ownership names are empty, uid/gid and timestamps are zero; gzip has no
source filename or timestamp. Unsupported USTAR paths fail rather than generating
extended headers. Packaging limits are 10000 entries, 1 GiB expanded including
headers and padding, and 256 MiB compressed. These are packaging guards as well as extractor
constraints; they do not replace the client's independent validation.

`create DIRECTORY VERSION` writes compact, stable JSON without a trailing newline
to `DIRECTORY/update-manifest.json`. It considers only the three exact supported
archive names for that version. ZIPs, macOS standalone tarballs, other versions,
unknown targets, checksums, and signatures are ignored. Selected archives must be
regular, non-symlink files in bounds. The command hashes their bytes but does not
authenticate arbitrary input archives or inspect their contents; use the package
commands on trusted build outputs first.

## Release Pipeline

1. Validate the public key and packaging tests, build both native macOS executables
   with the same embedded key/version, and assemble the universal app.
2. Stamp the bundle version, sign the app with Developer ID and hardened runtime,
   verify its signature, and create the existing manual-install ZIP.
3. Notarize the ZIP, staple and validate the bundle, rebuild the ZIP from that
   final bundle, and check Gatekeeper acceptance.
4. Package the **final signed, stapled bundle** as the updater USTAR tar.gz. No
   app files are modified after signing/stapling. The tar does not replace the ZIP.
5. Package optional Linux executables, create the manifest, check the PEM/public
   key match, sign the exact JSON, verify the signature, and require 64 raw bytes.
6. Generate the SBOM, checksums, Sigstore sidecars, and archive build provenance.
   Publish all assets together, refusing missing manifest/signature outputs.

The existing per-architecture macOS executable tarballs remain manual-download
assets, not updater targets. Apple code signing and notarization are independent
of the manifest signature and remain required for the app.

Artifact download uses `executable-*`. A future Linux build job must emit
`herdr-gpui-x86_64-unknown-linux-gnu` or
`herdr-gpui-aarch64-unknown-linux-gnu` at its artifact root and be added to the
package job's dependencies. The packaging loop detects those exact filenames.
That builder must embed the same release version and public key and validate its
own architecture, linkage, and runtime support. No speculative Linux GPUI build
matrix is included here.

### Publication Security

Publish increasing `YYYYMMDD.NN` versions **in order**. The workflow uses
`gh release create --latest`; its concurrency group serializes only the same ref,
not all release versions. Rerunning an old tag or finishing an older release after
a newer one can move GitHub's latest pointer backward. Clients reject downgrades,
but a stale latest pointer can prevent them discovering a newer release. This
pipeline does not enforce global release ordering. Coordinate releases and never
overwrite published signed assets. HTTPS and GitHub account security remain
important; a valid old signature alone does not prove freshness.

## Dry Runs And Verification

Dispatch Release with `dry_run: true` to build and package without Apple signing,
notarization, Ed25519 signing, Sigstore signing, or publication. The public key is
still required; no private update key is loaded and no manifest is created or
uploaded. Non-tag builds embed `00000000.00`, which is intentionally not a valid
updater release version. Dry-run output does not prove real installation works.

Run the standalone contract tests locally:

```sh
OPENSSL="$(brew --prefix openssl@3)/bin/openssl" \
  python3 -m unittest discover -s scripts -p 'test_update_manifest.py' -v
actionlint .github/workflows/release.yml .github/workflows/updater.yml
zizmor --offline .github/workflows/release.yml .github/workflows/updater.yml
git diff --check
```

On Linux use `OPENSSL=/usr/bin/openssl`. `.github/workflows/updater.yml` runs these
Python tests and the updater-only Rust harness on Linux and macOS without secrets
or a desktop:

```sh
python3 scripts/test-updater.py
# Use already cached dependencies without network access:
python3 scripts/test-updater.py --offline
```

The harness uses the real updater sources and the pinned toolchain/dependencies,
without GPUI's desktop build dependencies. On Linux it also exercises a signed
helper handoff, actual standalone replacement/relaunch, tampering refusal,
cancellation, and rollback in private temporary installations under a test HOME.
It does not build or test the complete Linux GUI or prove macOS Gatekeeper acceptance.

## Runtime And Recovery

Release builds check on startup, every six hours while idle, and on request.
Local builds without a valid release version and embedded key never check.
Checking/downloading/extraction and subprocess waits run off the UI thread.
Cancel is acknowledged after the current operation releases its staging resources;
a stalled HTTP read can delay cancellation until its request deadline (up to ten
minutes for an archive). No cancelled download is installed.

Linux installation requires an ordinary user-owned executable under `HOME`, such
as `~/.local/bin/herdr-gpui`, with safe ancestor permissions. Root, system locations,
symlinked paths, hard-linked binaries, and known package-container environments
are refused. Supported archive targets are x86_64 GNU and aarch64 GNU Linux.
On macOS the app must be user-owned and its parent writable without elevation;
the usual root/admin-owned `/Applications` parent is allowed. The installed and
candidate bundles must have matching Developer ID teams, Herdr's bundle identifier,
and the exact expected versions.

Installation re-extracts the authenticated archive after the GUI releases the
restart barrier. Original OS-string arguments and working directory are preserved.
The daemon is never signalled or stopped. A previous installation is retained in
an adjacent `.herdr-update-*` directory; post-commit failure details are recorded
in its `install-result.txt`. Rename or relaunch-spawn failures attempt restoration.
There is no post-launch health acknowledgement or automatic rollback if the new
process starts successfully and later crashes. Retain recovery copies until the
new version is confirmed working; cleanup is manual.

## Native QA

The GPUI **QA > Show app update available** action presents safe synthetic
update state, without network requests, installing files, or changing real update
preferences. Verify modal focus, keyboard isolation, dismissal, long labels, and
narrow-window clipping. Preview actions must not initiate a real update. No
external framework or signing credentials should be needed for synthetic QA.

Before declaring installation production-ready, exercise two distinct signed,
notarized versions on both macOS architectures with an active desktop. Use an
isolated test channel/key or deliberately coordinate production releases; do not
edit a signed bundle to fake an older version.

1. Install A and confirm its identity and Gatekeeper acceptance.
2. Publish B's final archives and signed manifest. Confirm checks offer B but do
   not install without approval; cancellation must leave A usable.
3. Approve installation, then verify authentication, replacement, relaunch, version
   display, and a subsequent no-update result. Existing daemon sessions must survive.
4. In the isolated harness, test changed JSON/archive bytes, wrong keys, truncated
   signatures, invalid paths, oversized inputs, offline/404 responses, read-only
   destinations, translocation, cancellation, and interrupted replacement. Failures
   must leave a usable installation and an actionable message.
5. When a Linux builder exists, separately test supported standalone installation
   locations, executable mode, replacement/relaunch, and failure recovery on both
   supported architectures. Do not infer Linux success from macOS or Python tests.

Record OS, architecture, versions, observed UI, and outcomes. Headless tests are
not a substitute for these native two-version checks.
