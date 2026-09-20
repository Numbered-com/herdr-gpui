//! Blocking release transport and authentication. Run only on a worker thread.
//! The caller supplies the compile-time HERDR_UPDATE_PUBLIC_KEY, never a secret.

use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const MANIFEST_LIMIT: usize = 64 * 1024;
const ARCHIVE_LIMIT: u64 = 256 * 1024 * 1024;
const METADATA_LIMIT: usize = 1024 * 1024;
const LATEST_URL: &str = "https://api.github.com/repos/penso/herdr-gpui/releases/latest";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) schema: u32,
    pub(super) version: String,
    pub(super) assets: Vec<Asset>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Asset {
    pub(super) target: String,
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Offer {
    pub(super) manifest: Manifest,
    pub(super) asset: Asset,
    pub(super) manifest_bytes: Vec<u8>,
    pub(super) signature: Vec<u8>,
}

pub(super) fn parse_version(value: &str) -> Option<(u32, u8)> {
    let bytes = value.as_bytes();
    if bytes.len() != 11
        || bytes[8] != b'.'
        || !bytes[..8].iter().chain(&bytes[9..]).all(u8::is_ascii_digit)
    {
        return None;
    }
    let date: u32 = value[..8].parse().ok()?;
    let year = date / 10000;
    let month = date / 100 % 100;
    let day = date % 100;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (year != 0 && (1..=days).contains(&day)).then_some((date, value[9..].parse().ok()?))
}

pub(super) fn target() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("universal-apple-darwin")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "gnu"
    )) {
        Some("aarch64-unknown-linux-gnu")
    } else {
        None
    }
}

fn hex32(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 {
        return Err("Expected 64 lowercase hexadecimal characters".into());
    }
    let mut result = [0; 32];
    for (out, pair) in result.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let digit = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err("Expected lowercase hexadecimal characters".to_string()),
        };
        *out = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(result)
}

fn asset_name(version: &str, target: &str) -> Result<String, String> {
    let suffix = match target {
        "universal-apple-darwin" => "macos-universal.app",
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => target,
        _ => return Err("Unsupported update target".into()),
    };
    Ok(format!("herdr-gpui-{version}-{suffix}.tar.gz"))
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    if manifest.schema != 1 || parse_version(&manifest.version).is_none() {
        return Err("Invalid update manifest schema or version".into());
    }
    if manifest.assets.is_empty() || manifest.assets.len() > 3 {
        return Err("Invalid update manifest asset count".into());
    }
    for (index, asset) in manifest.assets.iter().enumerate() {
        if asset.name != asset_name(&manifest.version, &asset.target)?
            || manifest.assets[..index]
                .iter()
                .any(|other| other.target == asset.target)
        {
            return Err("Invalid update asset name or duplicate target".into());
        }
        validate_digest(asset)?;
    }
    Ok(())
}

fn validate_digest(asset: &Asset) -> Result<[u8; 32], String> {
    if asset.size == 0 || asset.size > ARCHIVE_LIMIT {
        return Err("Update archive size is outside permitted bounds".into());
    }
    hex32(&asset.sha256)
}

pub(super) fn verify_manifest(
    bytes: &[u8],
    signature: &[u8],
    key_hex: &str,
    expected_version: &str,
) -> Result<Manifest, String> {
    if bytes.len() > MANIFEST_LIMIT || parse_version(expected_version).is_none() {
        return Err("Invalid manifest size or expected version".into());
    }
    let key =
        VerifyingKey::from_bytes(&hex32(key_hex)?).map_err(|_| "Invalid update public key")?;
    let signature = Signature::from_slice(signature).map_err(|_| "Invalid signature length")?;
    key.verify_strict(bytes, &signature)
        .map_err(|_| "Update manifest signature verification failed")?;
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid update manifest: {error}"))?;
    validate_manifest(&manifest)?;
    if manifest.version != expected_version {
        return Err("Update manifest version does not match release".into());
    }
    Ok(manifest)
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Update cancelled".into())
    } else {
        Ok(())
    }
}

fn release_url(version: &str, name: &str) -> String {
    format!("https://github.com/penso/herdr-gpui/releases/download/{version}/{name}")
}

fn allowed_url(value: &str) -> bool {
    let Ok(uri) = value.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && uri.authority().is_some_and(|authority| {
            matches!(
                authority.as_str(),
                "api.github.com" | "github.com" | "release-assets.githubusercontent.com"
            )
        })
        && !value.contains('#')
}

#[derive(Clone, Copy)]
enum RequestProfile {
    Metadata,
    Archive,
}

fn request_config(profile: RequestProfile) -> ureq::config::Config {
    let (total, body) = match profile {
        RequestProfile::Metadata => (120, 30),
        RequestProfile::Archive => (600, 600),
    };
    // ureq body/per-call timeouts are total deadlines, not idle-read limits.
    // Its standard transport exposes no independent per-read timeout: cancellation
    // can await the remaining body deadline if the current read stalls.
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(total)))
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .timeout_recv_body(Some(Duration::from_secs(body)))
        .build()
}

fn request(
    url: &str,
    profile: RequestProfile,
    cancel: &AtomicBool,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let agent: ureq::Agent = request_config(profile).into();
    let mut url = url.to_owned();
    for redirects in 0..=5 {
        cancelled(cancel)?;
        if !allowed_url(&url) {
            return Err("Update URL is outside the permitted HTTPS hosts".into());
        }
        let response = agent
            .get(&url)
            .header("User-Agent", "herdr-gpui-updater")
            .header(
                "Accept",
                if url == LATEST_URL {
                    "application/vnd.github+json"
                } else {
                    "application/octet-stream"
                },
            )
            .header("Accept-Encoding", "identity")
            .call()
            .map_err(|_| "Update HTTPS request failed")?;
        match response.status().as_u16() {
            200 => return Ok(response),
            301 | 302 | 303 | 307 | 308 if redirects < 5 => {
                // GitHub download redirects are absolute. Reject relative URLs rather
                // than introducing an additional URL resolution policy.
                url = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 8192)
                    .ok_or("Missing or invalid update redirect")?
                    .to_owned();
            }
            status => return Err(format!("Update request returned HTTP {status}")),
        }
    }
    Err("Too many update redirects".into())
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        cancelled(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("Reading update: {error}"))?;
        if count == 0 {
            cancelled(cancel)?;
            return Ok(bytes);
        }
        if count > limit.saturating_sub(bytes.len()) {
            return Err("Update response exceeds size limit".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

fn parse_release(bytes: &[u8]) -> Result<Release, String> {
    if bytes.len() > METADATA_LIMIT {
        return Err("Release metadata exceeds size limit".into());
    }
    let release: Release = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid GitHub release metadata: {error}"))?;
    if release.draft || release.prerelease || parse_version(&release.tag_name).is_none() {
        return Err("GitHub release is not a stable date-versioned release".into());
    }
    if release.assets.len() > 128 {
        return Err("GitHub release has too many assets".into());
    }
    for (index, asset) in release.assets.iter().enumerate() {
        if asset.name.is_empty()
            || asset.name.len() > 255
            || !asset
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || asset.name == "."
            || asset.name == ".."
            || asset.size == 0
            || asset.size > ARCHIVE_LIMIT
            || asset.browser_download_url != release_url(&release.tag_name, &asset.name)
            || release.assets[..index]
                .iter()
                .any(|other| other.name == asset.name)
        {
            return Err("Invalid GitHub release asset metadata".into());
        }
    }
    Ok(release)
}

pub(super) fn check(
    current_version: &str,
    key_hex: &str,
    cancel: &AtomicBool,
) -> Result<Option<Offer>, String> {
    cancelled(cancel)?;
    let current =
        parse_version(current_version).ok_or("Current build has no valid release version")?;
    VerifyingKey::from_bytes(&hex32(key_hex)?).map_err(|_| "Invalid update public key")?;
    let Some(target) = target() else {
        return Ok(None);
    };
    let metadata = read_bounded(
        request(LATEST_URL, RequestProfile::Metadata, cancel)?
            .into_body()
            .into_reader(),
        METADATA_LIMIT,
        cancel,
    )?;
    let release = parse_release(&metadata)?;
    if parse_version(&release.tag_name).ok_or("Invalid release version")? <= current {
        return Ok(None);
    }
    for (name, limit) in [
        ("update-manifest.json", MANIFEST_LIMIT as u64),
        ("update-manifest.sig", 64),
    ] {
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or("Release is missing signed update metadata")?;
        if asset.size > limit || (name == "update-manifest.sig" && asset.size != 64) {
            return Err("Invalid signed metadata size".into());
        }
    }
    let fetch = |name, limit| {
        read_bounded(
            request(
                &release_url(&release.tag_name, name),
                RequestProfile::Metadata,
                cancel,
            )?
            .into_body()
            .into_reader(),
            limit,
            cancel,
        )
    };
    let manifest_bytes = fetch("update-manifest.json", MANIFEST_LIMIT)?;
    let signature = fetch("update-manifest.sig", 64)?;
    let manifest = verify_manifest(&manifest_bytes, &signature, key_hex, &release.tag_name)?;
    let asset = manifest
        .assets
        .iter()
        .find(|asset| asset.target == target)
        .ok_or("Release has no update for this platform")?
        .clone();
    if !release
        .assets
        .iter()
        .any(|metadata| metadata.name == asset.name && metadata.size == asset.size)
    {
        return Err("Signed archive does not match GitHub release metadata".into());
    }
    cancelled(cancel)?;
    Ok(Some(Offer {
        manifest,
        asset,
        manifest_bytes,
        signature,
    }))
}

fn copy_archive(
    mut reader: impl Read,
    mut writer: impl Write,
    asset: &Asset,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    let expected_hash = validate_digest(asset)?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    cancelled(cancel)?;
    progress(0, asset.size);
    loop {
        cancelled(cancel)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("Reading archive: {error}"))?;
        cancelled(cancel)?;
        if count == 0 {
            break;
        }
        if count as u64 > asset.size - total {
            return Err("Update archive exceeds signed size".into());
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| format!("Writing archive: {error}"))?;
        hash.update(&buffer[..count]);
        total += count as u64;
        progress(total, asset.size);
    }
    if total != asset.size || hash.finalize().as_slice() != expected_hash {
        return Err("Update archive size or SHA-256 mismatch".into());
    }
    cancelled(cancel)
}

fn create_archive(destination: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(destination)
        .map_err(|error| format!("Creating update archive: {error}"))
}

/// Requires an offer authenticated by `check`/`verify_manifest`. On error, the
/// caller owns cleanup of its private staging directory, including partial files.
/// Cancellation is checked between reads; a stalled read can wait for the
/// remaining ten-minute request deadline. Never wait for this worker on the UI thread.
pub(super) fn download(
    offer: &Offer,
    destination: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    cancelled(cancel)?;
    validate_manifest(&offer.manifest)?;
    if !offer.manifest.assets.contains(&offer.asset)
        || Some(offer.asset.target.as_str()) != target()
    {
        return Err("Update offer asset does not match manifest or platform".into());
    }
    let response = request(
        &release_url(&offer.manifest.version, &offer.asset.name),
        RequestProfile::Archive,
        cancel,
    )?;
    cancelled(cancel)?;
    let mut file = create_archive(destination)?;
    copy_archive(
        response.into_body().into_reader(),
        &mut file,
        &offer.asset,
        cancel,
        progress,
    )?;
    file.sync_all()
        .map_err(|error| format!("Syncing update archive: {error}"))?;
    cancelled(cancel)
}

/// Recheck a staged archive against an authenticated asset immediately before
/// extraction. The caller must keep the staging directory private throughout.
pub(super) fn verify_archive(
    path: &Path,
    asset: &Asset,
    cancel: &AtomicBool,
) -> Result<(), String> {
    cancelled(cancel)?;
    let file = File::open(path).map_err(|error| format!("Opening update archive: {error}"))?;
    copy_archive(file, std::io::sink(), asset, cancel, |_, _| {})
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn request_profiles_allow_slow_archives_with_finite_deadlines() {
        for (profile, total, body) in [
            (RequestProfile::Metadata, 120, 30),
            (RequestProfile::Archive, 600, 600),
        ] {
            let config = request_config(profile);
            let timeouts = config.timeouts();
            assert_eq!(timeouts.global, Some(Duration::from_secs(total)));
            assert_eq!(timeouts.recv_body, Some(Duration::from_secs(body)));
            assert_eq!(timeouts.resolve, Some(Duration::from_secs(10)));
            assert_eq!(timeouts.connect, Some(Duration::from_secs(10)));
            assert_eq!(timeouts.send_request, Some(Duration::from_secs(15)));
            assert_eq!(timeouts.recv_response, Some(Duration::from_secs(15)));
            // A short per-call timeout would also truncate the entire body.
            assert_eq!(timeouts.per_call, None);
            assert!(config.https_only());
            assert_eq!(config.max_redirects(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn archive_creation_is_private_and_never_overwrites() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicU64;

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = loop {
            let directory = std::env::temp_dir().join(format!(
                "herdr-release-permissions-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&directory) {
                Ok(()) => break directory,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("Creating test directory: {error}"),
            }
        };
        let path = directory.join("archive.tar.gz");
        let mut file = create_archive(&path).unwrap();
        // Do not mutate the process-global umask in a parallel test. The explicit
        // creation mode excludes group/other bits regardless of the current umask.
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o077, 0);
        file.write_all(b"original").unwrap();
        assert!(create_archive(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        drop(file);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn fixture() -> Manifest {
        Manifest {
            schema: 1,
            version: "20260920.01".into(),
            assets: vec![Asset {
                target: "universal-apple-darwin".into(),
                name: "herdr-gpui-20260920.01-macos-universal.app.tar.gz".into(),
                size: 3,
                sha256: hex(&Sha256::digest(b"abc")),
            }],
        }
    }

    fn signed(bytes: &[u8]) -> Result<Manifest, String> {
        let key = SigningKey::from_bytes(&[42; 32]);
        verify_manifest(
            bytes,
            &key.sign(bytes).to_bytes(),
            &hex(key.verifying_key().as_bytes()),
            "20260920.01",
        )
    }

    #[test]
    fn date_versions_are_canonical_real_dates() {
        for value in ["20260920.01", "20000229.99", "20240229.00", "00010101.01"] {
            assert!(parse_version(value).is_some(), "{value}");
        }
        for value in [
            "00000000.00",
            "00000101.01",
            "19000229.01",
            "20260229.01",
            "20260431.01",
            "20261301.01",
            "20260900.01",
            "20260920.1",
            "v20260920.01",
            "20260920.001",
            "２０２６0920.01",
        ] {
            assert!(parse_version(value).is_none(), "{value}");
        }
        assert!(parse_version("20260920.02") > parse_version("20260920.01"));
    }

    #[test]
    fn authentication_binds_exact_bytes_key_and_version() {
        let manifest = fixture();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        assert_eq!(signed(&bytes).unwrap(), manifest);
        let key = SigningKey::from_bytes(&[42; 32]);
        let signature = key.sign(&bytes).to_bytes();
        let public = hex(key.verifying_key().as_bytes());
        assert!(verify_manifest(&bytes, &signature[..63], &public, &manifest.version).is_err());
        assert!(verify_manifest(&bytes, &signature, &public, "20260920.02").is_err());
        let wrong_key = SigningKey::from_bytes(&[43; 32]);
        assert!(
            verify_manifest(
                &bytes,
                &signature,
                &hex(wrong_key.verifying_key().as_bytes()),
                &manifest.version
            )
            .is_err()
        );
        let mut changed = bytes.clone();
        changed.push(b' ');
        assert!(verify_manifest(&changed, &signature, &public, &manifest.version).is_err());
        assert!(signed(&vec![b' '; MANIFEST_LIMIT + 1]).is_err());
        assert!(signed(b"not json").is_err());
    }

    #[test]
    fn signed_manifest_still_requires_strict_policy() {
        let base = fixture();
        let mut invalid = Vec::new();
        let mut value = base.clone();
        value.schema = 2;
        invalid.push(value);
        let mut value = base.clone();
        value.assets.clear();
        invalid.push(value);
        let mut value = base.clone();
        value.assets.push(value.assets[0].clone());
        invalid.push(value);
        let mut value = base.clone();
        value.assets[0].target = "unknown".into();
        invalid.push(value);
        let mut value = base.clone();
        value.assets[0].name = "../archive".into();
        invalid.push(value);
        let mut value = base.clone();
        value.assets[0].size = ARCHIVE_LIMIT + 1;
        invalid.push(value);
        let mut value = base.clone();
        value.assets[0].size = 0;
        invalid.push(value);
        let mut value = base.clone();
        value.assets[0].sha256 = "A".repeat(64);
        invalid.push(value);
        for manifest in invalid {
            assert!(signed(&serde_json::to_vec(&manifest).unwrap()).is_err());
        }
        let mut value = serde_json::to_value(base).unwrap();
        value["extra"] = true.into();
        assert!(signed(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(signed(br#"{"schema":1,"schema":1,"version":"20260920.01","assets":[]}"#).is_err());
    }

    #[test]
    fn archive_reader_checks_size_digest_and_cancellation() {
        let asset = fixture().assets.remove(0);
        let cancel = AtomicBool::new(false);
        let mut output = Vec::new();
        let mut progress = Vec::new();
        copy_archive(&b"abc"[..], &mut output, &asset, &cancel, |done, total| {
            progress.push((done, total))
        })
        .unwrap();
        assert_eq!(output, b"abc");
        assert_eq!(progress, [(0, 3), (3, 3)]);
        for bytes in [&b"ab"[..], &b"abcd"[..], &b"abd"[..]] {
            assert!(copy_archive(bytes, std::io::sink(), &asset, &cancel, |_, _| {}).is_err());
        }
        assert!(
            copy_archive(&b"abc"[..], std::io::sink(), &asset, &cancel, |_, _| cancel
                .store(true, Ordering::Relaxed))
            .is_err()
        );
        assert!(read_bounded(&b"abc"[..], 3, &cancel).is_err());
        cancel.store(false, Ordering::Relaxed);
        assert_eq!(read_bounded(&b"abc"[..], 3, &cancel).unwrap(), b"abc");
        assert!(read_bounded(&b"abcd"[..], 3, &cancel).is_err());
    }

    #[test]
    fn redirects_require_exact_https_authorities() {
        for value in [
            LATEST_URL,
            "https://github.com/a",
            "https://release-assets.githubusercontent.com/a?signature=x",
        ] {
            assert!(allowed_url(value));
        }
        for value in [
            "http://github.com/a",
            "https://github.com.evil.test/a",
            "https://github.com@evil.test/a",
            "https://evil@github.com/a",
            "https://github.com:444/a",
            "https://github.com/a#fragment",
            "/relative",
            "https://objects.githubusercontent.com/a",
        ] {
            assert!(!allowed_url(value), "{value}");
        }
    }

    #[test]
    fn release_metadata_rejects_unstable_and_untrusted_names() {
        let value = serde_json::json!({"tag_name":"20260920.01", "draft":false, "prerelease":false, "assets":[{
            "name":"update-manifest.json", "size":100,
            "browser_download_url":release_url("20260920.01", "update-manifest.json")
        }]});
        assert!(parse_release(&serde_json::to_vec(&value).unwrap()).is_ok());
        for field in ["draft", "prerelease"] {
            let mut bad = value.clone();
            bad[field] = true.into();
            assert!(parse_release(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        for name in ["../escape", "bad/name", "..", "bad%20name"] {
            let mut bad = value.clone();
            bad["assets"][0]["name"] = name.into();
            assert!(parse_release(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = value.clone();
        bad["assets"][0]["browser_download_url"] = "https://evil.test/a".into();
        assert!(parse_release(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}
