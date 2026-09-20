# Security Policy

## Supported Versions

Herdr GPUI is pre-1.0 and has no published releases yet. Only the current
`main` branch receives fixes. Pin a commit if you need a stable reference.

## Reporting a Vulnerability

Report privately through GitHub's
[security advisory form](https://github.com/penso/herdr-gpui/security/advisories/new).
Please do not open a public issue for a suspected vulnerability.

Include the affected commit, your macOS and Rust versions, what an attacker
would gain, and a reproduction if you have one. Expect an initial reply within
a week; this is a small project without a paid on-call rotation.

## Scope

This repository is a **client**. It connects over a Unix domain socket to a
Herdr daemon that you already run, and it owns no terminal processes, no
server state, and no network listener.

In scope:

- Parsing and framing of daemon messages in `crates/herdr-protocol` — decoder
  limits, bounds, and malformed or hostile payloads.
- Socket discovery and connection handling in `crates/herdr-client`, including
  which socket paths are trusted and how a session is selected.
- Anything in `crates/herdr-gpui` that lets terminal content escape its pane:
  unintended command execution, clipboard or notification writes, or path
  handling from daemon-supplied strings.

Out of scope here, and belonging upstream at
[herdrdev/herdr](https://github.com/herdrdev/herdr):

- The daemon itself, its socket permissions, and its session model.
- Anything requiring an attacker who can already run code as your user — at
  that point they can talk to the daemon directly, with or without this client.

Dependency advisories are handled by Dependabot and the `cargo-deny` CI job;
open a normal issue for those rather than a private advisory.

## Verifying a Release

Each base artifact (universal DMG, experimental Linux archive, and CycloneDX
SBOM) has checksums, a Sigstore signature/certificate, and GitHub provenance.
Checksums alone are not authentication:

| Files | Claim |
| --- | --- |
| `.sha256`, `.sha512` | The bytes are intact |
| `.sig` + `.crt` | Sigstore: this repository's workflow produced them |
| GitHub attestation | Provenance: which workflow, commit, and runner built them |
| Optional local `.asc` | Supplemental GPG approval, using a key CI never holds |

```sh
# Exact asset set, SHA256/SHA512, Sigstore and GitHub provenance
bash scripts/verify-release.sh --version v0.1.0

# Basic provenance check (the script also pins workflow, main ref, and tag SHA)
gh attestation verify Herdr-0.1.0-universal-apple-darwin.dmg \
  --repo penso/herdr-gpui
```

The verification script requires Python 3.11+, `gh`, and cosign 2.x. It requires
the exact workflow identity
`https://github.com/penso/herdr-gpui/.github/workflows/release.yml@refs/heads/main`,
GitHub's OIDC issuer, and the commit identified by the lightweight `vX.Y.Z` tag.
Use `--sha FULL_COMMIT_SHA` to pin a separately reviewed source commit rather
than trusting the current tag mapping. These are manual **main-branch** runs;
the certificate identity is not a tag ref. Verification requires network access
to GitHub and Sigstore; no transparency-log checks are disabled.

`SHA256SUMS` contains exactly 15 entries: the three base files and each file's
`.sha256`, `.sha512`, `.sig`, and `.crt` sidecars. The immutable release has those
15 files plus `SHA256SUMS`. Publication checks the complete set and downloaded
draft bytes before making it public. Enable GitHub immutable releases before
dispatch; the workflow never replaces a tag, release, or published asset.

### Optional GPG Approval

The maintainer's primary public key is at <https://pen.so/gpg.asc>, fingerprint
`3103 20A8 CC1C 5BA8 6AD0 9040 C045 1BAD F764 9BBF`. Confirm the **full signing
subkey fingerprint** through a trusted channel as well: it can differ from the
primary fingerprint. HTTPS and a successful GPG verification alone do not prove
that the expected person signed.

`bash scripts/gpg-sign-release.sh v0.1.0 ./release-approval FULL_SIGNING_FINGERPRINT`
downloads and verifies the immutable release, then saves detached signatures in
`release-approval/gpg/`. It never uploads, clobbers, or modifies a release. Share
these supplemental signatures separately; they are not promised release assets.
No GPG private key is stored in CI. To verify downloaded supplemental signatures:

```sh
bash scripts/verify-release.sh --version v0.1.0 --directory ./release-approval \
  --gpg-key ./maintainer-public.asc --gpg-fingerprint FULL_SIGNING_FINGERPRINT
```

GPG verification imports into a temporary isolated keyring, requires a signature
for every base artifact, and checks the full actual signing fingerprint from
`VALIDSIG`. Missing or wrong signatures fail rather than being skipped.

## Build and Release Hardening

- Release execution is gated by [zizmor](https://docs.zizmor.sh) in the distinct
  `Release Workflow Security` job before validation/builds. Only manual dispatches
  on `main` in `penso/herdr-gpui`, with both actor and rerun actor `penso`, qualify.
  CI's required `Workflow Security` check remains a separate check.
- Every action is pinned to a commit SHA; Dependabot updates them with a
  seven-day cooldown so an automated PR cannot pull in a freshly compromised
  release on day one.
- Workflows grant no permissions by default; each job requests only what it
  needs, and checkouts do not persist credentials.
- Release builds restore no dependency cache — a poisoned cache entry would
  otherwise be signed and notarized along with everything else.
- The macOS bundle is signed with a Developer ID certificate under the
  hardened runtime with no entitlements, then notarized and stapled.
- A secret-free metadata job generates the CycloneDX 1.5 SBOM using pinned
  cargo-cyclonedx 0.5.9. Its metadata calls enforce `--locked`; the graph unions
  default-feature ARM macOS, Intel macOS, and x86_64 Linux dependencies, including
  build-time dependencies. Platform scopes merge with `required` taking precedence
  over `optional`, then `excluded`; absent scope is treated as `required`. Other
  component fields must match, including identities, hashes, and licenses.
  This is a Cargo dependency inventory, not an inventory
  of OS libraries or proof of exact linked code.
- Apple secrets are signing-step-only; temporary keys and keychain are removed
  before later steps. A separate owner-approved `release` environment job signs
  with OIDC and attests the final bytes, with no Apple secrets or build execution.
  Publication and Homebrew updates require protected environment approval too.
  These controls depend on configuring GitHub environment reviewers/branch rules;
  see [Release Operations](README.md#release-operations).
