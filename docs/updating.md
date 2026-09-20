# macOS Updates With Sparkle

Release bundles contain the complete Sparkle 2.10.0 framework in
`Herdr.app/Contents/Frameworks/Sparkle.framework`. Rust loads it dynamically from
the main app bundle; standalone executables do not carry an updater. The
checked-in `assets/macos/Info.plist` and local `just bundle` remain updater-free.

The stable feed is a GitHub release asset, not a GitHub Pages site:

```text
https://github.com/penso/herdr-gpui/releases/latest/download/appcast.xml
```

## Administrator Setup

An administrator must configure these repository settings before a real release:

| Setting | Kind | Contents |
| --- | --- | --- |
| `SPARKLE_PUBLIC_ED_KEY` | Actions variable | Canonical base64 encoding of the 32-byte Ed25519 public key |
| `SPARKLE_PRIVATE_KEY` | Actions secret | Exact base64 text exported by Sparkle `generate_keys -x`, not another base64 encoding of that file |

The existing Apple Developer ID and notarization secrets are still required:
`APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_API_KEY_P8`,
`APPLE_API_KEY_ID`, and `APPLE_API_ISSUER_ID`. Sigstore uses GitHub OIDC separately.
Protect release tags and workflow changes; anyone who can change trusted release
code can potentially use its signing credentials.

Run the following deliberately on a trusted administrator Mac, from this repo.
These are setup instructions, not commands run automatically by packaging.
`generate_keys` creates or reuses the named account's key in the login Keychain.

```sh
work="$(mktemp -d)"
bash scripts/fetch-sparkle.sh "$work/sparkle"
"$work/sparkle/bin/generate_keys" --account herdr-gpui
gh variable set SPARKLE_PUBLIC_ED_KEY --repo penso/herdr-gpui \
  --body "$("$work/sparkle/bin/generate_keys" --account herdr-gpui -p)"
(umask 077; "$work/sparkle/bin/generate_keys" --account herdr-gpui -x "$work/private-key")
gh secret set SPARKLE_PRIVATE_KEY --repo penso/herdr-gpui < "$work/private-key"
rm -f "$work/private-key"
rm -rf "$work"
```

Keep a securely encrypted backup of the signing key independent of CI. Never
commit it, paste it into logs, pass it as a CLI argument, or use shell tracing
around it. A real release fails before compilation if either setting is missing
or the public key is malformed. A mismatched keypair is rejected during appcast
validation: Sparkle's generator otherwise only warns and omits the archive's
signature. Do not replace the public key casually: existing installs trust the
key embedded in their installed bundle.

## Release Pipeline

`scripts/fetch-sparkle.sh DESTINATION` requires a destination that does not exist
and an existing parent directory. It downloads only:

```text
https://github.com/sparkle-project/Sparkle/releases/download/2.10.0/Sparkle-2.10.0.tar.xz
SHA256: c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c
```

Downloads are HTTPS-only, time-bounded, and limited to 100 MiB. The checksum is
verified before extraction or tool execution. The release uses the framework and
tools from that same distribution, preserving symlinks, executable permissions,
resources, helpers, and license files. Updating Sparkle requires explicitly
reviewing and changing both the version URL and checksum, as well as checking
helper layout, entitlements, and tool flags again.

The workflow stamps these keys into the staged release plist. The checked-in
plist also disables automatic installation for safe local QA, but contains no
feed or signing key and therefore never starts the updater:

| Key | Value |
| --- | --- |
| `SUFeedURL` | The stable GitHub URL above |
| `SUPublicEDKey` | `vars.SPARKLE_PUBLIC_ED_KEY` |
| `SUEnableAutomaticChecks` | `YES` |
| `SUAutomaticallyUpdate` | `NO` |
| `SUAllowsAutomaticUpdates` | `NO` |
| `SUVerifyUpdateBeforeExtraction` | `YES` |
| `SURequireSignedFeed` | `YES` |

Automatic checks are enabled by default, but installation requires an explicit
user action every time. `SUAllowsAutomaticUpdates = NO` also removes the option
to enable silent automatic updates; `SUAutomaticallyUpdate = NO` alone would
only set a default. These options are supported by Sparkle 2.10.0; see
[customization](https://sparkle-project.org/documentation/customization/).

Signing proceeds inside out: Downloader XPC (preserving its distributed
entitlements), Installer XPC, Autoupdate, Updater.app, Sparkle.framework, then
Herdr.app. All use the same Developer ID identity and hardened runtime. The
workflow verifies with `codesign --verify --deep --strict`; it never signs with
`--deep` and does not disable library validation.

The app is notarized and stapled, and its ZIP is rebuilt before feed generation.
Only that final ZIP goes into a fresh appcast directory. The pinned
`generate_appcast --ed-key-file - --maximum-versions 1 --maximum-deltas 0` reads
the private key from stdin. `SURequireSignedFeed` causes the tool to sign the feed
as well as the archive. The pinned `sign_update --verify --ed-key-file -`
verifies the feed; structural checks require exactly the current release,
a signed full archive, its final length, and its version-specific URL:

```text
https://github.com/penso/herdr-gpui/releases/download/VERSION/herdr-gpui-VERSION-macos-universal.app.zip
```

`appcast.xml` is uploaded and published with the ZIP. Its exact bytes are also
covered by SHA256/SHA512 checksums and Sigstore signatures. Do not reformat,
append to, or hand-edit a generated signed feed. It must be regenerated and
re-signed after any change. Build provenance covers the executable archives,
not the feed. The publish step refuses a missing feed and marks the new release
as latest so the stable URL resolves to it. Publish increasing `YYYYMMDD.NN`
versions in order; do not rerun an old release as latest. Each feed deliberately
contains just one version, with no delta or historical compatibility branches.

## Security Boundaries

Sparkle verifies archive signatures before extraction and validates signed feed
metadata. Developer ID signing and notarization remain separate checks. GitHub
HTTPS hosts the feed and immutable-by-convention versioned ZIP URLs; do not
overwrite published artifacts. Neither Sparkle nor this pipeline updates the
Herdr daemon or its terminal sessions.

Sparkle's default signed-feed failure expiration remains in effect (20 days).
After that interval, Sparkle can use its restricted recovery behavior; signed
feed enforcement is not an indefinite fail-closed promise. See Sparkle's
[security settings](https://sparkle-project.org/documentation/customization/#security-settings).

With pre-extraction verification enabled, losing the EdDSA private key cannot
be recovered by simply publishing a ZIP signed with a different key. Sparkle's
[key rotation](https://sparkle-project.org/documentation/#rotating-signing-keys)
fallback requires a Developer ID signed DMG, which this ZIP-only pipeline does
not generate. Plan a reviewed recovery release or manual reinstall rather than
weakening verification. Do not rotate Apple and EdDSA identities simultaneously.

## Dry Runs

Dispatch Release with `dry_run: true` to build the universal bundle and embed
the verified framework without Apple signing, notarization, or publication.
Without a public key it omits all updater configuration and reports this
explicitly. With a valid public key it stamps the production updater settings;
use care when launching that bundle because it points at the production feed.
A malformed configured public key fails even a dry run.

Dry runs do not load the Sparkle private key, generate an appcast, or upload an
unsigned feed. The existing checksum/Sigstore steps still run for other assets.
An unsigned dry-run bundle is not proof that hardened-runtime dynamic loading,
Gatekeeper, installation, or relaunch works. Ordinary local builds do not fetch
Sparkle and do not require release credentials.

## Required Native QA

### Preview The Offer

```sh
just bundle-updater-preview
open target/release/Herdr.app
```

Choose **QA > Show app update available**. This displays Sparkle's real standard
update window with synthetic version `99991231.99 (QA preview)` and inline plain
text. Install, Skip, Later (when shown), and closing the window only dismiss it.
No updater session exists for this preview, no release notes are fetched, and
the synthetic archive URL uses `example.invalid`. The automatic-install checkbox
is disabled by the bundle's `SUAllowsAutomaticUpdates = NO`, so the preview
cannot change real update preferences or skip a real release. Reopening the menu
entry closes the previous preview first. Local QA needs no signing credentials.

The QA-only state construction uses a private Sparkle initializer and a deprecated
appcast-item initializer: it checks the exact framework version and selectors,
and refuses other versions rather than guessing. Re-audit this path when changing
the pinned Sparkle release. Production update checking uses public APIs only.

### Verify Real Installation

Before declaring update support production-ready, exercise two distinct signed,
notarized versions on both Apple Silicon and Intel with an active desktop.
Use isolated test builds and a separate signed test feed/key, or deliberately
coordinate the first two production releases. Never mutate a signed app's plist
after signing to fake an old version.

1. Install version A in `/Applications`; confirm its framework loads under
   hardened runtime, and inspect nested signatures and Gatekeeper acceptance.
2. Publish version B's final stapled ZIP and signed feed. Confirm automatic
   checking and the app's manual check action find B without installing it.
3. Confirm there is no automatic-install opt-in, cancellation leaves A running,
   and an old stored automatic-update preference cannot enable silent installs.
4. Explicitly install B. Verify download, signature validation, replacement,
   relaunch, displayed version, and a subsequent no-update result. Confirm daemon
   processes and existing terminals survive the GUI replacement.
5. In the isolated test feed, test a modified feed, modified ZIP, wrong signing
   key, offline/404 responses, and a read-only/translocated app. Confirm safe
   failure with useful UI and no replacement. Restore the correctly signed feed.
6. Confirm a clean local build and a keyless dry-run bundle leave updating
   unavailable without startup errors.

Record OS/architecture, versions, observed UI, and results. Syntax checks and
workflow audits do not substitute for this two-version native test.

## Linux And Standalone Builds

Sparkle is an AppKit framework, not a Rust cross-platform updater. The macOS-only
module uses `objc2` to load the framework from the installed bundle and retains
its controller on the UI thread; Sparkle performs network/download/install work
asynchronously. Bare executables do not search for or download a framework.

The non-macOS implementation does not initialize an updater. Linux distribution
support must choose an appropriate installation format first: package-managed
applications should normally update through their package manager, while an
AppImage could use a separate signed AppImage update mechanism. No Linux updater
or Linux release packaging is added by this integration.
