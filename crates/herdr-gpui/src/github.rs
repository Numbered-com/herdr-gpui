//! Native GitHub transport and device authorization. No credential subprocesses.
use crate::{Error, Result};
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox as Secret, SecretString};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    io::Read,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

mod credentials;
pub(super) const PLAINTEXT_WARNING: &str = "WARNING: plaintext credential storage is enabled. GitHub tokens are unencrypted on disk; software running as you and backups can read them.";
pub(super) const DEVELOPMENT_WARNING: &str = "WARNING: this development build does not use the macOS Keychain. GitHub tokens are unencrypted beside the GUI config; software running as you and backups can read them.";
pub(super) const KEYCHAIN_NOTICE: &str =
    "Credentials are saved in macOS Keychain. macOS may ask you to unlock or approve access.";

const LIMIT: u64 = 2 * 1024 * 1024;
#[cfg(target_os = "macos")]
const SERVICE: &str = "dev.herdr.gpui.github";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "github.com";
pub(super) const VERIFY_URL: &str = "https://github.com/login/device";
const SETUP_MESSAGE: &str = "Connect with Herdr GPUI's GitHub App. Optionally set [github] oauth_client_id in config-gpui.toml, or HERDR_GITHUB_OAUTH_CLIENT_ID, to another GitHub App or OAuth App public client ID with Device Flow enabled. Reload GUI config after file edits.";

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn response<T: DeserializeOwned>(mut response: ureq::http::Response<ureq::Body>) -> Result<T> {
    match response.status().as_u16() {
        200..=299 => {}
        401 => return Err(Error::GitHubAuthentication),
        403 => return Err(Error::GitHubForbidden),
        429 => return Err(Error::GitHubRateLimit),
        status => return Err(Error::GitHubStatus(status)),
    }
    // Allocate the bounded capacity up front: no reallocations leave old body
    // fragments behind, and partial reads are wiped even on I/O errors.
    let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT as usize + 1));
    response
        .body_mut()
        .as_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(Error::GitHubRead)?;
    if bytes.len() > LIMIT as usize {
        return Err(Error::GitHubSize);
    }
    serde_json::from_slice(&bytes).map_err(Error::github_json)
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic())
}

fn resolve_token(
    gh: Option<SecretString>,
    github: Option<SecretString>,
    saved: impl FnOnce() -> Result<Option<SecretString>>,
) -> Result<SecretString> {
    let token = match gh
        .filter(|s| !s.expose_secret().trim().is_empty())
        .or_else(|| github.filter(|s| !s.expose_secret().trim().is_empty()))
    {
        Some(token) => Some(token),
        None => saved()?,
    }
    .ok_or(Error::GitHubAuthentication)?;
    let trimmed = token.expose_secret().trim();
    if !valid_token(trimmed) {
        return Err(Error::GitHubToken);
    }
    if trimmed.len() == token.expose_secret().len() {
        Ok(token)
    } else {
        Ok(trimmed.into())
    }
}

fn credential_bytes(bytes: Vec<u8>) -> Result<SecretString> {
    let bytes = Zeroizing::new(bytes);
    // Validate by borrowing so invalid UTF-8 never escapes in FromUtf8Error.
    std::str::from_utf8(&bytes)
        .map(SecretString::from)
        .map_err(Error::GitHubEncoding)
}

fn environment_token(name: &str) -> Result<Option<SecretString>> {
    // Own and wipe even non-Unicode environment values. The process environment
    // itself is outside this allocation's lifetime and is not erased here.
    std::env::var_os(name)
        .map(|value| credential_bytes(value.into_encoded_bytes()))
        .transpose()
}

/// Where a saved GitHub token lives for this build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Store {
    /// No persistent store: `GH_TOKEN` / `GITHUB_TOKEN` only.
    #[default]
    Environment,
    /// The app-specific macOS login Keychain entry.
    Keychain,
    /// A private `0600` file beside the GUI config.
    File,
}

// A development build is unsigned and gets a fresh code identity on every rebuild,
// so macOS would re-prompt for the Keychain item's ACL on every run. Only the
// signed release pipeline sets `HERDR_RELEASE_VERSION`, so only it uses Keychain.
const KEYCHAIN: bool = cfg!(target_os = "macos") && crate::RELEASE_BUILD;
// Those development builds have no other secure store to fall back on, so the
// private file is their default. Every other platform keeps it an explicit opt-in.
const FILE_DEFAULT: bool = cfg!(target_os = "macos") && !KEYCHAIN;
// An unsigned build must never reach the Keychain, whatever else changes here.
const _: () = assert!(!KEYCHAIN || crate::RELEASE_BUILD);

impl Store {
    pub(super) fn select(config: &crate::config::Config) -> Self {
        Self::choose(
            config.github.allow_plaintext_credentials,
            KEYCHAIN,
            FILE_DEFAULT,
        )
    }

    const fn choose(opted_in: bool, keychain: bool, file_default: bool) -> Self {
        if keychain {
            Self::Keychain
        } else if opted_in || file_default {
            Self::File
        } else {
            Self::Environment
        }
    }

    /// Credential handling shown in the GitHub menu, so storage is never implicit.
    pub(super) fn note(self, connected: bool) -> Option<Note> {
        match self {
            Self::File if FILE_DEFAULT => Some(Note::Warning(DEVELOPMENT_WARNING)),
            Self::File => Some(Note::Warning(PLAINTEXT_WARNING)),
            Self::Keychain if !connected => Some(Note::Info(KEYCHAIN_NOTICE)),
            Self::Keychain | Self::Environment => None,
        }
    }
}

pub(super) enum Note {
    Warning(&'static str),
    Info(&'static str),
}

#[cfg(target_os = "macos")]
fn keychain_token() -> Result<Option<SecretString>> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => credential_bytes(bytes).map(Some),
        Err(e) if e.code() == -25300 => Ok(None),
        Err(error) => Err(Error::KeychainRead(error)),
    }
}

// `Store::Keychain` is never selected off macOS; the stubs keep the match total.
#[cfg(not(target_os = "macos"))]
fn keychain_token() -> Result<Option<SecretString>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn keychain_save(token: Option<&SecretString>) -> Result<()> {
    use security_framework::passwords::{delete_generic_password, set_generic_password};
    let result = match token {
        Some(token) => set_generic_password(SERVICE, ACCOUNT, token.expose_secret().as_bytes()),
        None => delete_generic_password(SERVICE, ACCOUNT),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if token.is_none() && e.code() == -25300 => Ok(()),
        Err(error) => Err(Error::KeychainWrite(error)),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_save(_: Option<&SecretString>) -> Result<()> {
    Err(Error::CredentialPolicy)
}

fn saved_token(store: Store) -> Result<Option<SecretString>> {
    match store {
        Store::Environment => Ok(None),
        Store::Keychain => keychain_token(),
        Store::File => credentials::read(&credential_directory()?),
    }
}

fn load_token(store: Store) -> Result<Option<SecretString>> {
    let gh = environment_token("GH_TOKEN")?;
    let github = if gh
        .as_ref()
        .is_none_or(|s| s.expose_secret().trim().is_empty())
    {
        environment_token("GITHUB_TOKEN")?
    } else {
        None
    };
    let saved = || saved_token(store);
    if gh
        .as_ref()
        .is_none_or(|s| s.expose_secret().trim().is_empty())
        && github
            .as_ref()
            .is_none_or(|s| s.expose_secret().trim().is_empty())
    {
        return saved()?
            .map(|token| resolve_token(None, None, || Ok(Some(token))))
            .transpose();
    }
    resolve_token(gh, github, saved).map(Some)
}

fn credential_directory() -> Result<std::path::PathBuf> {
    crate::config::Config::path()?
        .parent()
        .map(std::path::Path::to_owned)
        .ok_or(Error::CredentialDirectory)
}

fn save(token: Option<&SecretString>, store: Store) -> Result<()> {
    match store {
        Store::Keychain => keychain_save(token),
        // Removal stays allowed without an opt-in, so a file written under an
        // earlier policy is still cleaned up by an explicit sign-out.
        Store::Environment => credentials::store(&credential_directory()?, token, false),
        Store::File => credentials::store(&credential_directory()?, token, true),
    }
}

pub(super) fn graphql(
    token: &SecretString,
    query: &str,
    variables: Value,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
    cooldown: &mut Option<Duration>,
) -> Result<Value> {
    let deadline = Instant::now() + timeout;
    if cancelled() {
        return Err(Error::PrCancelled);
    }
    if cancelled() {
        return Err(Error::PrCancelled);
    }
    let body = serde_json::json!({"query":query,"variables":variables}).to_string();
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or(Error::PrTimeout)?;
    let reply = agent(timeout)
        .post("https://api.github.com/graphql")
        .header("User-Agent", "Herdr-GPUI")
        .header("Accept", "application/vnd.github+json")
        .header("Content-Type", "application/json")
        .header("Authorization", authorization(token)?)
        .send(body.as_bytes())
        .map_err(Error::GitHubNetwork)?;
    *cooldown = pr_cooldown(
        reply.status().as_u16(),
        reply.headers(),
        std::time::SystemTime::now(),
    );
    let result: Value = response(reply)?;
    if cancelled() {
        return Err(Error::PrCancelled);
    }
    if result.get("errors").is_some() {
        if result["errors"].as_array().is_some_and(|errors| {
            errors.iter().any(|error| {
                matches!(
                    error["type"].as_str(),
                    Some("RATE_LIMITED" | "FORBIDDEN" | "UNAUTHORIZED")
                )
            })
        }) {
            *cooldown = Some(Duration::from_secs(3600));
        }
        return Err(Error::GitHubQuery);
    }
    Ok(result)
}

fn authorization(token: &SecretString) -> Result<ureq::http::HeaderValue> {
    let mut text = Secret::new(Box::new(String::with_capacity(
        7 + token.expose_secret().len(),
    )));
    text.expose_secret_mut().push_str("Bearer ");
    text.expose_secret_mut().push_str(token.expose_secret());
    let mut header =
        ureq::http::HeaderValue::from_str(text.expose_secret()).map_err(Error::GitHubHeader)?;
    header.set_sensitive(true);
    Ok(header)
}

fn pr_cooldown(
    status: u16,
    headers: &ureq::http::HeaderMap,
    now: std::time::SystemTime,
) -> Option<Duration> {
    if !matches!(status, 401 | 403 | 429) {
        return None;
    }
    let seconds = |name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
    };
    let reset = seconds("x-ratelimit-reset").map(|reset| {
        reset.saturating_sub(
            now.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
    });
    Some(Duration::from_secs(
        seconds("retry-after")
            .into_iter()
            .chain(reset)
            .max()
            .unwrap_or(3600)
            .clamp(300, 86400),
    ))
}

fn oauth<T: DeserializeOwned>(path: &str, fields: &[(&str, &str)], timeout: Duration) -> Result<T> {
    // ureq owns form serialization and HTTP/TLS buffers; their copies cannot be
    // zeroized by this module. Never log request fields or raw response errors.
    response(
        agent(timeout)
            .post(format!("https://github.com/login/{path}"))
            .header("User-Agent", "Herdr-GPUI")
            .header("Accept", "application/json")
            .send_form(fields.iter().copied())
            .map_err(Error::GitHubNetwork)?,
    )
}

#[derive(Debug, Deserialize)]
struct Device {
    device_code: SecretString,
    user_code: SecretString,
    verification_uri: String,
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}
fn default_interval() -> u64 {
    5
}

impl Device {
    fn validate(self) -> Result<Self> {
        let user_code = self.user_code.expose_secret();
        if !valid_token(self.device_code.expose_secret())
            || user_code.len() > 32
            || user_code.is_empty()
            || !user_code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.verification_uri != VERIFY_URL
            || !(1..=900).contains(&self.expires_in)
            || !(1..=900).contains(&self.interval)
        {
            return Err(Error::GitHubDevice);
        }
        Ok(self)
    }
}

enum Reply {
    Device(Device, String, Instant),
    Pending(bool),
    Token(SecretString),
    SignedOut,
    Authenticated(Arc<SecretString>),
}

pub(super) struct Profile {
    pub login: String,
    pub avatar: Option<Arc<gpui::Image>>,
    pub token: Arc<SecretString>,
    avatar_updates: Option<crate::avatars::AvatarUpdates>,
}

fn profile(token: Arc<SecretString>) -> Result<Profile> {
    #[derive(Deserialize)]
    struct User {
        login: String,
        avatar_url: String,
    }
    let user: User = response(
        agent(Duration::from_secs(15))
            .get("https://api.github.com/user")
            .header("User-Agent", "Herdr-GPUI")
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", authorization(&token)?)
            .call()
            .map_err(Error::GitHubNetwork)?,
    )?;
    if crate::avatars::github_repo(&format!("https://github.com/{}/profile", user.login)).is_none()
    {
        return Err(Error::GitHubProfile);
    }
    // The image transport receives no Authorization header and follows no redirects.
    let (avatar, avatar_updates) = crate::avatars::profile_avatar(&user.avatar_url);
    Ok(Profile {
        login: user.login,
        avatar,
        token,
        avatar_updates,
    })
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<SecretString>,
    token_type: Option<String>,
    error: Option<String>,
}

fn token_reply(value: TokenResponse) -> Result<Reply> {
    match value.error.as_deref() {
        Some("authorization_pending") => Ok(Reply::Pending(false)),
        Some("slow_down") => Ok(Reply::Pending(true)),
        Some("expired_token") => Err(Error::GitHubExpired),
        Some("access_denied") => Err(Error::GitHubDenied),
        Some(_) => Err(Error::GitHubAuthorization),
        None => {
            let token = value
                .access_token
                .filter(|t| valid_token(t.expose_secret()))
                .ok_or(Error::GitHubToken)?;
            if value
                .token_type
                .as_deref()
                .is_none_or(|t| !t.eq_ignore_ascii_case("bearer"))
            {
                return Err(Error::GitHubTokenType);
            }
            Ok(Reply::Token(token))
        }
    }
}

struct Flow {
    copied_until: Option<Instant>,
    client: String,
    device: Arc<Device>,
    interval: u64,
    deadline: Instant,
    next: Instant,
}

#[derive(Default)]
pub(super) struct Auth {
    flow: Option<Flow>,
    incoming: Option<mpsc::Receiver<Result<Reply>>>,
    cancelled: bool,
    committing: bool,
    signout_pending: bool,
    initialized: bool,
    signed_out: bool,
    reload_pending: bool,
    store: Store,
    profile_incoming: Option<mpsc::Receiver<Result<Option<Profile>>>>,
    pub profile: Option<Profile>,
    pub message: Option<String>,
    pub failed: bool,
    credential_cleanup: bool,
}

impl Auth {
    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn connected_fixture() -> Self {
        Self {
            initialized: true,
            profile: Some(Profile {
                login: "fixture-user".into(),
                avatar: None,
                token: Arc::new("fixture-token".into()),
                avatar_updates: None,
            }),
            ..Self::default()
        }
    }
    pub fn connected(&self) -> bool {
        self.profile.is_some()
    }
    pub fn loading_profile(&self) -> bool {
        self.reload_pending || self.profile_incoming.is_some()
    }
    pub fn initialize(&mut self, config: &crate::config::Config) -> bool {
        self.initialize_with(Store::select(config))
    }
    fn initialize_with(&mut self, store: Store) -> bool {
        let changed = !self.initialized || self.store != store;
        self.store = store;
        self.initialized = true;
        if !changed || self.signed_out {
            return false;
        }
        self.profile = None;
        self.flow = None;
        self.cancelled = true;
        self.reload_pending = true;
        self.failed = false;
        self.message =
            Some("Checking GitHub account under the updated credential policy...".into());
        // Drain old workers before reloading, so rapid policy changes stay bounded.
        // Neither a late profile nor an accepted write can restore the old session.
        true
    }
    fn load_profile_with(
        &mut self,
        token: Option<Arc<SecretString>>,
        load: impl FnOnce(Option<Arc<SecretString>>, Store) -> Result<Option<Profile>> + Send + 'static,
    ) {
        self.failed = false;
        let store = self.store;
        let (tx, rx) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("herdr-github-profile".into())
            .spawn(move || {
                let _ = tx.send(load(token, store));
            }) {
            Ok(_) => self.profile_incoming = Some(rx),
            Err(_) => {
                self.failed = true;
                self.message = Some("Could not start GitHub profile worker.".into());
            }
        }
    }
    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn fixture(waiting: bool) -> Self {
        let now = Instant::now();
        Self {
            flow: waiting.then(|| Flow {
                copied_until: None,
                client: "fixture".into(),
                device: Arc::new(Device {
                    device_code: "fixture".into(),
                    user_code: "ABCD-1234".into(),
                    verification_uri: VERIFY_URL.into(),
                    expires_in: 900,
                    interval: 900,
                }),
                interval: 900,
                deadline: now + Duration::from_secs(900),
                next: now + Duration::from_secs(900),
            }),
            message: Some(
                if waiting {
                    "Open GitHub and enter this code. Waiting for authorization..."
                } else {
                    SETUP_MESSAGE
                }
                .into(),
            ),
            ..Self::default()
        }
    }
    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn requesting_fixture() -> Self {
        let (_, incoming) = mpsc::sync_channel(1);
        Self {
            incoming: Some(incoming),
            initialized: true,
            message: Some("Requesting GitHub sign-in code...".into()),
            ..Self::default()
        }
    }
    pub fn busy(&self) -> bool {
        self.incoming.is_some() || self.flow.is_some() || self.signout_pending
    }
    pub fn code(&self) -> Option<&str> {
        // The device user code is intentionally displayed, unlike access tokens.
        self.flow
            .as_ref()
            .filter(|f| Instant::now() < f.deadline)
            .map(|f| f.device.user_code.expose_secret())
    }
    pub fn can_sign_out(&self) -> bool {
        self.connected() || self.credential_cleanup
    }
    pub fn copied(&self) -> bool {
        self.flow.as_ref().is_some_and(|flow| {
            flow.copied_until
                .is_some_and(|until| Instant::now() < until)
        })
    }
    pub fn copy_code(&mut self) -> Option<&str> {
        let flow = self.flow.as_mut().filter(|f| Instant::now() < f.deadline)?;
        flow.copied_until = Some(Instant::now() + Duration::from_secs(3));
        Some(flow.device.user_code.expose_secret())
    }
    fn launch(&mut self, work: impl FnOnce() -> Result<Reply> + Send + 'static) {
        let (tx, rx) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("herdr-github-auth".into())
            .spawn(move || {
                let _ = tx.send(work());
            }) {
            Ok(_) => self.incoming = Some(rx),
            Err(_) => {
                self.flow = None;
                self.committing = false;
                self.failed = true;
                self.message = Some("Could not start GitHub authentication worker.".into());
            }
        }
    }
    pub fn start(&mut self, config: &crate::config::Config) {
        if self.busy() || self.connected() || self.loading_profile() {
            return;
        }
        self.store = Store::select(config);
        self.initialized = true;
        self.failed = false;
        if self.store == Store::Environment {
            self.failed = true;
            self.message =
                Some("No secure credential store configured. To accept unencrypted token storage, set [github] allow_plaintext_credentials = true and reload GUI config. Otherwise use GH_TOKEN / GITHUB_TOKEN.".into());
            return;
        }
        let client = match config.github.client_id() {
            Ok(Some(client)) => client,
            Ok(None) => {
                self.message = Some(SETUP_MESSAGE.into());
                return;
            }
            Err(error) => {
                self.failed = true;
                self.message = Some(error.to_string());
                return;
            }
        };
        self.cancelled = false;
        self.signed_out = false;
        self.profile_incoming = None;
        self.message = Some("Requesting GitHub sign-in code...".into());
        self.launch(move || {
            let started = Instant::now();
            let device = oauth::<Device>(
                "device/code",
                // OAuth Apps use repo; GitHub Apps ignore scope and use their
                // registered permissions and installation repository access.
                &[("client_id", &client), ("scope", "repo")],
                Duration::from_secs(15),
            )?
            .validate()?;
            Ok(Reply::Device(device, client, started))
        });
    }
    pub fn cancel(&mut self) {
        // A token is persisted only after the UI accepts the completed flow. Once
        // accepted, Keychain writes finish off-thread even if the menu closes.
        if self.committing || self.signout_pending {
            return;
        }
        self.cancelled = true;
        self.failed = false;
        self.flow = None;
        self.message = Some("GitHub sign-in cancelled.".into());
    }
    pub fn sign_out(&mut self) {
        self.credential_cleanup = true;
        self.failed = false;
        self.initialized = true;
        self.signed_out = true;
        self.reload_pending = false;
        self.profile = None;
        self.profile_incoming = None;
        self.flow = None;
        // Serialize deletion after an accepted write, but suppress credentials now.
        if !self.committing {
            self.incoming = None;
        }
        self.signout_pending = true;
        self.cancelled = false;
        self.message = Some("Signed out locally. Removing saved GitHub credential...".into());
    }
    pub fn poll(&mut self) -> bool {
        let store = self.store;
        self.poll_with_store(move |token| save(token, store))
    }
    /// Credential backend chosen for this build and configuration.
    pub(super) fn store(&self) -> Store {
        self.store
    }
    fn poll_with_store(
        &mut self,
        persist: impl FnOnce(Option<&SecretString>) -> Result<()> + Send + 'static,
    ) -> bool {
        self.poll_with(persist, |token, store| {
            let token = match token {
                Some(token) => Some(token),
                None => load_token(store)?.map(Arc::new),
            };
            token.map(profile).transpose()
        })
    }
    fn poll_with(
        &mut self,
        persist: impl FnOnce(Option<&SecretString>) -> Result<()> + Send + 'static,
        load: impl FnOnce(Option<Arc<SecretString>>, Store) -> Result<Option<Profile>> + Send + 'static,
    ) -> bool {
        if self.signout_pending && !self.committing {
            self.signout_pending = false;
            self.committing = true;
            self.launch(move || {
                persist(None)?;
                Ok(Reply::SignedOut)
            });
            return true;
        }
        if self.reload_pending && !self.busy() && self.profile_incoming.is_none() {
            self.reload_pending = false;
            self.cancelled = false;
            self.load_profile_with(None, load);
            return true;
        }
        let mut changed = false;
        if let Some(profile) = &mut self.profile
            && let Some(updates) = &profile.avatar_updates
        {
            match updates.try_recv() {
                Ok(image) => {
                    profile.avatar = Some(image);
                    profile.avatar_updates = None;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Disconnected) => profile.avatar_updates = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.profile_incoming {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(Error::GitHubWorker("profile"))),
            };
            if let Some(result) = result {
                self.profile_incoming = None;
                if !self.reload_pending {
                    match result {
                        Ok(profile) => {
                            self.profile = profile;
                            self.failed = false;
                            self.message = None;
                        }
                        Err(error) => {
                            self.credential_cleanup = true;
                            self.profile = None;
                            self.failed = true;
                            self.message = Some(error.to_string());
                        }
                    }
                }
                changed = true;
            }
        }
        if let Some(rx) = &self.incoming {
            let reply = match rx.try_recv() {
                Ok(reply) => Some(reply),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(Error::GitHubWorker("authentication")))
                }
            };
            if let Some(reply) = reply {
                self.incoming = None;
                changed = true;
                if self.signout_pending || self.reload_pending {
                    self.committing = false;
                    return true;
                }
                if !self.cancelled {
                    match reply {
                        Ok(Reply::Device(device, client, started)) => {
                            let now = Instant::now();
                            self.flow = Some(Flow {
                                copied_until: None,
                                client,
                                deadline: started + Duration::from_secs(device.expires_in),
                                next: now + Duration::from_secs(device.interval),
                                interval: device.interval,
                                device: Arc::new(device),
                            });
                            self.message = Some(
                                "Open GitHub and enter this code. Waiting for authorization..."
                                    .into(),
                            );
                        }
                        Ok(Reply::Pending(slow)) => {
                            if let Some(flow) = &mut self.flow {
                                if slow {
                                    flow.interval = flow.interval.saturating_add(5);
                                }
                                flow.next = Instant::now() + Duration::from_secs(flow.interval);
                            }
                        }
                        Ok(Reply::Token(token)) => {
                            if self
                                .flow
                                .as_ref()
                                .is_some_and(|f| Instant::now() < f.deadline)
                            {
                                self.committing = true;
                                self.credential_cleanup = true;
                                self.message = Some("Saving GitHub credential...".into());
                                self.launch(move || {
                                    persist(Some(&token))?;
                                    Ok(Reply::Authenticated(Arc::new(token)))
                                });
                            } else {
                                self.failed = true;
                                self.message = Some("GitHub code expired. Sign in again.".into());
                            }
                            self.flow = None;
                        }
                        Ok(Reply::SignedOut) => {
                            self.credential_cleanup = false;
                            self.committing = false;
                            self.message = Some("Signed out for this app session. Saved credential removed. Environment tokens are suppressed until app restart; GitHub grants are not revoked.".into());
                        }
                        Ok(Reply::Authenticated(token)) => {
                            self.committing = false;
                            self.message = Some("Loading GitHub profile...".into());
                            self.load_profile_with(Some(token), load);
                        }
                        Err(error) => {
                            self.flow = None;
                            self.committing = false;
                            self.failed = true;
                            self.message = Some(error.to_string());
                        }
                    }
                }
            }
        }
        if let Some(flow) = &mut self.flow
            && flow
                .copied_until
                .is_some_and(|until| Instant::now() >= until)
        {
            flow.copied_until = None;
            changed = true;
        }
        if self.incoming.is_none()
            && let Some(flow) = &self.flow
        {
            let now = Instant::now();
            if now >= flow.deadline {
                self.flow = None;
                self.failed = true;
                self.message = Some("GitHub code expired. Sign in again.".into());
                return true;
            }
            if now >= flow.next {
                let client = flow.client.clone();
                // Share secret ownership with the in-flight worker, not plaintext.
                let device = Arc::clone(&flow.device);
                let timeout = (flow.deadline - now).min(Duration::from_secs(15));
                self.launch(move || {
                    token_reply(oauth(
                        "oauth/access_token",
                        &[
                            ("client_id", &client),
                            ("device_code", device.device_code.expose_secret()),
                            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                        ],
                        timeout,
                    )?)
                });
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn avatar_refresh_is_scoped_to_the_verified_profile() {
        let mut auth = Auth::connected_fixture();
        let (tx, rx) = mpsc::sync_channel(1);
        auth.profile.as_mut().unwrap().avatar_updates = Some(rx);
        let image = Arc::new(gpui::Image::empty());
        tx.send(image.clone()).unwrap();
        assert!(auth.poll_with_store(|_| panic!("avatar must not access credentials")));
        assert!(Arc::ptr_eq(
            auth.profile.as_ref().unwrap().avatar.as_ref().unwrap(),
            &image
        ));
        let (tx, rx) = mpsc::sync_channel(1);
        auth.profile.as_mut().unwrap().avatar_updates = Some(rx);
        auth.sign_out();
        assert!(!auth.connected());
        assert!(tx.send(image.clone()).is_err());
        auth.profile = Auth::connected_fixture().profile;
        assert!(auth.profile.as_ref().unwrap().avatar.is_none());
        let (tx, rx) = mpsc::sync_channel(1);
        auth.profile.as_mut().unwrap().avatar_updates = Some(rx);
        auth.signed_out = false;
        auth.initialized = false;
        auth.initialize(&crate::config::Config::default());
        assert!(!auth.connected());
        assert!(tx.send(image).is_err());
    }

    #[test]
    fn copy_feedback_is_scoped_to_live_flow_and_expires() {
        let mut auth = Auth::fixture(true);
        assert!(!auth.copied());
        assert!(!auth.can_sign_out());
        assert_eq!(auth.copy_code(), Some("ABCD-1234"));
        assert!(auth.copied());
        auth.flow.as_mut().unwrap().copied_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(auth.poll_with_store(|_| panic!("no storage for copy")));
        assert!(!auth.copied());
        auth.copy_code();
        auth.cancel();
        assert!(auth.copy_code().is_none());
        assert!(!auth.copied());
        auth = Auth::fixture(true);
        assert!(!auth.copied());
        auth.flow.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
        assert!(auth.code().is_none());
        assert!(auth.copy_code().is_none());
        assert!(auth.poll_with_store(|_| panic!("expired code must not write")));
        assert!(auth.failed);
        assert!(!auth.busy());
        assert!(Auth::connected_fixture().can_sign_out());
    }

    #[test]
    fn only_a_signed_release_build_uses_the_keychain() {
        // A signed release build keeps the Keychain whatever the config says.
        assert_eq!(Store::choose(false, true, false), Store::Keychain);
        assert_eq!(Store::choose(true, true, false), Store::Keychain);
        // An unsigned development build gets a new code identity on every rebuild,
        // so it uses the private file instead of re-prompting for Keychain access.
        assert_eq!(Store::choose(false, false, true), Store::File);
        assert_eq!(Store::choose(true, false, true), Store::File);
        // Everywhere else unencrypted storage stays an explicit opt-in.
        assert_eq!(Store::choose(false, false, false), Store::Environment);
        assert_eq!(Store::choose(true, false, false), Store::File);
        let mut config = crate::config::Config::default();
        #[cfg(target_os = "macos")]
        if !crate::RELEASE_BUILD {
            assert_eq!(Store::select(&config), Store::File);
        }
        config.github.allow_plaintext_credentials = true;
        assert_eq!(Store::select(&config), Store::choose(true, KEYCHAIN, false));
    }
    #[test]
    fn credential_notes_state_where_tokens_are_kept() {
        assert!(Store::Environment.note(false).is_none());
        assert!(Store::Environment.note(true).is_none());
        assert!(matches!(Store::Keychain.note(false), Some(Note::Info(_))));
        assert!(
            Store::Keychain.note(true).is_none(),
            "a connected account already proved Keychain access"
        );
        for connected in [false, true] {
            let Some(Note::Warning(text)) = Store::File.note(connected) else {
                panic!("unencrypted storage must always warn");
            };
            assert!(text.starts_with("WARNING: "));
        }
    }
    fn load_fixture_profile(auth: &mut Auth, store: Store, profile: Option<Profile>) {
        assert!(auth.poll_with(
            |_| panic!("policy reload must not change stored credentials"),
            move |token, policy| {
                assert!(token.is_none(), "must resolve under the new policy");
                assert_eq!(policy, store);
                assert_eq!(thread::current().name(), Some("herdr-github-profile"));
                Ok(profile)
            },
        ));
        let result = auth
            .profile_incoming
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(result).ok().unwrap();
        auth.profile_incoming = Some(rx);
        assert!(auth.poll_with(
            |_| panic!("profile does not persist tokens"),
            |_, _| panic!("no duplicate profile request"),
        ));
    }

    #[test]
    fn enabling_plaintext_reloads_saved_token_but_explicit_signout_stays_suppressed() {
        let mut auth = Auth::default();
        // Drive the backend directly: which one a configuration selects depends on
        // the build, but every transition between them must behave the same way.
        assert!(auth.initialize_with(Store::Environment));
        load_fixture_profile(&mut auth, Store::Environment, None);
        assert!(!auth.connected());
        assert!(auth.initialize_with(Store::File));
        load_fixture_profile(&mut auth, Store::File, Auth::connected_fixture().profile);
        assert!(auth.connected());
        assert!(
            !auth.initialize_with(Store::File),
            "unchanged policy must not poll"
        );
        auth.sign_out();
        auth.poll_with(
            |token| {
                assert!(token.is_none());
                Ok(())
            },
            |_, _| panic!("signed out"),
        );
        let reply = auth
            .incoming
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        deliver(&mut auth, reply);
        auth.poll_with(|_| panic!("already removed"), |_, _| panic!("signed out"));
        for store in [Store::Environment, Store::File, Store::Keychain] {
            assert!(!auth.initialize_with(store));
            assert_eq!(auth.store(), store);
            assert!(auth.signed_out);
            assert!(!auth.loading_profile());
            assert!(!auth.connected());
            assert!(!auth.poll_with(
                |_| panic!("no store access"),
                |_, _| panic!("no credential reload")
            ));
        }
    }

    #[test]
    fn disabling_plaintext_clears_session_and_rejects_late_profile() {
        let mut auth = Auth::default();
        assert!(auth.initialize_with(Store::File));
        load_fixture_profile(&mut auth, Store::File, Auth::connected_fixture().profile);
        assert!(auth.connected());
        assert!(auth.initialize_with(Store::Environment));
        assert!(
            !auth.connected(),
            "old token cannot remain usable during reload"
        );
        assert!(!auth.signed_out, "policy changes are not explicit sign-out");
        load_fixture_profile(&mut auth, Store::Environment, None);
        assert!(!auth.connected());

        assert!(auth.initialize_with(Store::File));
        // A pending load from the opted-in policy must not win after opting out.
        auth.reload_pending = false;
        let (tx, rx) = mpsc::sync_channel(1);
        auth.profile_incoming = Some(rx);
        assert!(auth.initialize_with(Store::Environment));
        assert!(tx.send(Ok(Auth::connected_fixture().profile)).is_ok());
        assert!(auth.poll_with(
            |_| panic!("no store access"),
            |_, _| panic!("old worker must drain")
        ));
        assert!(!auth.connected());
        // An environment credential is still allowed under the new policy.
        load_fixture_profile(
            &mut auth,
            Store::Environment,
            Auth::connected_fixture().profile,
        );
        assert!(auth.connected());
    }

    #[test]
    fn policy_reload_drains_accepted_write_without_applying_its_token() {
        let mut auth = Auth::connected_fixture();
        auth.store = Store::File;
        auth.committing = true;
        deliver(
            &mut auth,
            Ok(Reply::Authenticated(Arc::new("late-fixture".into()))),
        );
        assert!(auth.initialize_with(Store::Environment));
        assert!(auth.poll_with(
            |_| panic!("write was already accepted"),
            |_, _| panic!("must drain the accepted write first"),
        ));
        assert!(!auth.committing);
        assert!(auth.reload_pending);
        assert!(!auth.connected());
        load_fixture_profile(&mut auth, Store::Environment, None);
        assert!(!auth.connected());
    }

    #[test]
    fn signout_discards_late_profile_and_auth_without_environment_reactivation() {
        let mut auth = Auth::connected_fixture();
        let (tx, rx) = mpsc::sync_channel(1);
        auth.profile_incoming = Some(rx);
        deliver(&mut auth, Ok(Reply::Token("late-fixture".into())));
        auth.sign_out();
        assert!(!auth.connected());
        assert!(!auth.loading_profile());
        assert!(auth.incoming.is_none());
        assert!(tx.send(Ok(Auth::connected_fixture().profile)).is_err());
        // Reload/reconnect cannot read environment or disk after explicit sign-out.
        auth.initialize(&crate::config::Config::default());
        assert!(!auth.loading_profile());
        auth.poll_with_store(|token| {
            assert!(token.is_none());
            Err(std::io::Error::other("mock removal failure").into())
        });
        let reply = auth
            .incoming
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        deliver(&mut auth, reply);
        auth.poll_with_store(|_| panic!("no second store operation"));
        assert!(!auth.connected());
        assert!(auth.failed);
        assert_eq!(auth.message.as_deref(), Some("mock removal failure"));
    }

    #[test]
    fn signout_serializes_after_accepted_write_and_discards_its_profile() {
        let mut auth = Auth::connected_fixture();
        auth.committing = true;
        deliver(
            &mut auth,
            Ok(Reply::Authenticated(Arc::new("late-fixture".into()))),
        );
        auth.sign_out();
        auth.cancel(); // Dismissal must not cancel credential removal.
        auth.poll_with_store(|_| panic!("write must complete before deletion"));
        assert!(!auth.connected());
        assert!(!auth.loading_profile());
        assert!(auth.signout_pending);
        auth.poll_with_store(|token| {
            assert!(token.is_none());
            Ok(())
        });
        let reply = auth
            .incoming
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        deliver(&mut auth, reply);
        auth.poll_with_store(|_| panic!("already removed"));
        assert!(!auth.busy());
        assert!(!auth.connected());
        assert!(
            auth.message
                .as_deref()
                .unwrap()
                .contains("Environment tokens are suppressed")
        );
    }

    #[test]
    fn profile_loading_success_error_and_idle_do_not_start_device_auth() {
        let mut auth = Auth::default();
        for result in [
            Ok(Auth::connected_fixture().profile),
            Err(std::io::Error::other("mock profile failure").into()),
            Ok(None),
        ] {
            let (tx, rx) = mpsc::sync_channel(1);
            tx.send(result).ok().unwrap();
            auth.profile_incoming = Some(rx);
            assert!(auth.loading_profile());
            assert!(auth.poll_with_store(|_| panic!("profile does not persist tokens")));
            assert!(!auth.loading_profile());
            assert!(auth.incoming.is_none());
            assert!(auth.flow.is_none());
            assert_eq!(auth.failed, auth.message.is_some());
        }
        assert!(!auth.connected());
    }

    fn device() -> Device {
        serde_json::from_str::<Device>(r#"{"device_code":"fixture-device", "user_code":"ABCD-1234", "verification_uri":"https://github.com/login/device", "expires_in":900, "interval":5}"#).unwrap().validate().unwrap()
    }
    #[test]
    fn setup_fixture_describes_public_config_and_environment_override() {
        let auth = Auth::fixture(false);
        assert_eq!(auth.message.as_deref(), Some(SETUP_MESSAGE));
        assert!(SETUP_MESSAGE.contains("[github] oauth_client_id"));
        assert!(SETUP_MESSAGE.contains("HERDR_GITHUB_OAUTH_CLIENT_ID"));
        assert!(SETUP_MESSAGE.contains("GitHub App or OAuth App public client ID"));
    }
    fn token_reply(value: Value) -> Result<Reply> {
        super::token_reply(serde_json::from_value(value).unwrap())
    }
    fn deliver(auth: &mut Auth, reply: Result<Reply>) {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(reply).ok().unwrap();
        auth.incoming = Some(rx);
    }
    fn waiting() -> Auth {
        let mut auth = Auth::default();
        deliver(
            &mut auth,
            Ok(Reply::Device(
                device(),
                "fixture-client".into(),
                Instant::now(),
            )),
        );
        assert!(auth.poll());
        auth
    }
    #[test]
    fn auth_priority_and_storage_errors_never_fall_back_silently() {
        assert_eq!(
            resolve_token(Some(" gh ".into()), Some("github".into()), || panic!(
                "must not read Keychain"
            ))
            .unwrap()
            .expose_secret(),
            "gh"
        );
        assert_eq!(
            resolve_token(Some(" ".into()), Some("github".into()), || panic!(
                "must not read Keychain"
            ))
            .unwrap()
            .expose_secret(),
            "github"
        );
        assert_eq!(
            resolve_token(None, None, || Ok(Some("saved".into())))
                .unwrap()
                .expose_secret(),
            "saved"
        );
        assert!(
            resolve_token(None, None, || Ok(None))
                .unwrap_err()
                .to_string()
                .contains("authentication required")
        );
        assert_eq!(
            resolve_token(None, None, || Err(std::io::Error::other("locked").into()))
                .unwrap_err()
                .to_string(),
            "locked"
        );
        assert!(resolve_token(Some("bad\nsecret".into()), None, || panic!()).is_err());
        assert!(resolve_token(None, None, || Ok(Some("  ".into()))).is_err());
        assert_eq!(
            resolve_token(Some(" \t".into()), Some("\n".into()), || Ok(Some(
                " saved ".into()
            )))
            .unwrap()
            .expose_secret(),
            "saved"
        );
        assert!(
            resolve_token(
                Some("bad\nsecret".into()),
                Some("valid".into()),
                || panic!()
            )
            .is_err()
        );
    }
    #[test]
    fn credentials_and_authorization_debug_are_redacted() {
        let token = credential_bytes(b"fixture-access-secret".to_vec()).unwrap();
        assert!(!format!("{token:?}").contains("fixture-access-secret"));
        let header = authorization(&token).unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), "Bearer fixture-access-secret");
        assert!(!format!("{header:?}").contains("fixture-access-secret"));
        let device = device();
        let debug = format!("{device:?}");
        assert!(!debug.contains("fixture-device"));
        assert!(!debug.contains("ABCD-1234"));
        let error = credential_bytes(b"private-invalid-secret\xff".to_vec()).unwrap_err();
        assert!(matches!(error, Error::GitHubEncoding(_)));
        assert_eq!(error.to_string(), "Invalid GitHub credential encoding.");
        assert!(authorization(&SecretString::from("private\nsecret")).is_err());
    }
    #[test]
    fn oauth_responses_deserialize_directly_to_redacted_secrets() {
        let reply = |body: &[u8]| {
            ureq::http::Response::builder()
                .status(200)
                .body(ureq::Body::builder().data(body.to_vec()))
                .unwrap()
        };
        let parsed: TokenResponse = response(reply(
            br#"{"access_token":"fixture-access-secret","token_type":"bearer"}"#,
        ))
        .unwrap();
        assert!(!format!("{parsed:?}").contains("fixture-access-secret"));
        let Reply::Token(token) = super::token_reply(parsed).unwrap() else {
            panic!()
        };
        assert_eq!(token.expose_secret(), "fixture-access-secret");
        for body in [
            br#"{"access_token":"private-secret","token_type":123}"#.as_slice(),
            br#"{"access_token":"private-secret","access_token":"duplicate"}"#,
            b"private-invalid-secret\xff",
        ] {
            assert_eq!(
                response::<TokenResponse>(reply(body))
                    .unwrap_err()
                    .to_string(),
                "Invalid GitHub JSON response."
            );
        }
        let parsed: Device = response(reply(br#"{"device_code":"fixture-device","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900}"#)).unwrap();
        assert_eq!(
            parsed.validate().unwrap().user_code.expose_secret(),
            "ABCD-1234"
        );
    }
    #[test]
    fn expired_token_reply_is_not_persisted() {
        let mut auth = waiting();
        auth.flow.as_mut().unwrap().deadline = Instant::now();
        deliver(&mut auth, Ok(Reply::Token("expired-secret".into())));
        auth.poll_with_store(|_| panic!("expired token must never be stored"));
        assert!(!auth.busy());
        assert!(auth.code().is_none());
        assert!(auth.message.as_ref().unwrap().contains("expired"));
    }
    #[test]
    fn pr_rate_limits_have_bounded_account_wide_cooldowns() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1000);
        let mut headers = ureq::http::HeaderMap::new();
        assert_eq!(pr_cooldown(200, &headers, now), None);
        for status in [401, 403, 429] {
            assert_eq!(
                pr_cooldown(status, &headers, now),
                Some(Duration::from_secs(3600))
            );
        }
        headers.insert("retry-after", "600".parse().unwrap());
        headers.insert("x-ratelimit-reset", "2200".parse().unwrap());
        assert_eq!(
            pr_cooldown(429, &headers, now),
            Some(Duration::from_secs(1200))
        );
        headers.insert("retry-after", "18446744073709551615".parse().unwrap());
        assert_eq!(
            pr_cooldown(429, &headers, now),
            Some(Duration::from_secs(86400))
        );
        headers.insert("retry-after", "invalid".parse().unwrap());
        headers.insert("x-ratelimit-reset", "0".parse().unwrap());
        assert_eq!(
            pr_cooldown(403, &headers, now),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn bounded_http_parsing_and_safe_errors() {
        let reply = |status, body: Vec<u8>| {
            ureq::http::Response::builder()
                .status(status)
                .body(ureq::Body::builder().data(body))
                .unwrap()
        };
        for (status, message) in [
            (401, "authentication required"),
            (403, "denied access"),
            (429, "rate limit"),
            (302, "request failed"),
            (500, "request failed"),
        ] {
            let error =
                response::<Value>(reply(status, b"private-error-secret".to_vec())).unwrap_err();
            assert!(error.to_string().contains(message));
            assert!(!error.to_string().contains("private-error-secret"));
        }
        assert_eq!(
            response::<Value>(reply(200, b"{\"ok\":true}".to_vec())).unwrap()["ok"],
            true
        );
        assert!(response::<Value>(reply(200, b"not-json".to_vec())).is_err());
        assert!(response::<Value>(reply(200, vec![b' '; LIMIT as usize + 1])).is_err());
        assert!(
            graphql(
                &"fixture".into(),
                "",
                Value::Null,
                Duration::from_secs(1),
                || true,
                &mut None
            )
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
    }
    #[test]
    fn device_validation_and_oauth_error_lifecycle() {
        for (key, value) in [
            ("verification_uri", serde_json::json!("https://evil.test")),
            ("expires_in", serde_json::json!(901)),
            ("interval", serde_json::json!(0)),
            ("user_code", serde_json::json!("bad\ncode")),
        ] {
            let mut v = serde_json::json!({"device_code":"fixture", "user_code":"ABCD-1234", "verification_uri":VERIFY_URL, "expires_in":900, "interval":5});
            v[key] = value;
            assert!(
                serde_json::from_value::<Device>(v)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(matches!(
            token_reply(serde_json::json!({"error":"authorization_pending"})),
            Ok(Reply::Pending(false))
        ));
        assert!(matches!(
            token_reply(serde_json::json!({"error":"slow_down"})),
            Ok(Reply::Pending(true))
        ));
        for error in [
            "expired_token",
            "access_denied",
            "incorrect_client_credentials",
            "unknown",
        ] {
            assert!(
                token_reply(
                    serde_json::json!({"error":error, "error_description":"private-secret"})
                )
                .err()
                .unwrap()
                .to_string()
                .find("private-secret")
                .is_none()
            );
        }
        assert!(
            token_reply(serde_json::json!({"access_token":"fixture", "token_type":"mac"})).is_err()
        );
        assert!(matches!(
            token_reply(serde_json::json!({"access_token":"fixture", "token_type":"bearer"})),
            Ok(Reply::Token(_))
        ));
    }
    #[test]
    fn pending_slowdown_expiry_and_cancel_do_not_store_stale_tokens() {
        let mut auth = waiting();
        assert_eq!(auth.code(), Some("ABCD-1234"));
        deliver(&mut auth, Ok(Reply::Pending(true)));
        auth.poll();
        assert_eq!(auth.flow.as_ref().unwrap().interval, 10);
        deliver(&mut auth, Ok(Reply::Pending(false)));
        auth.poll();
        assert_eq!(auth.flow.as_ref().unwrap().interval, 10);
        auth.flow.as_mut().unwrap().deadline = Instant::now();
        auth.poll();
        assert!(!auth.busy());
        assert!(auth.message.as_ref().unwrap().contains("expired"));
        for reply in [
            Reply::Token("fixture".into()),
            Reply::Device(device(), "client".into(), Instant::now()),
        ] {
            let mut auth = waiting();
            deliver(&mut auth, Ok(reply));
            auth.cancel();
            auth.poll_with_store(|_| panic!("cancelled token must never be stored"));
            assert!(!auth.busy());
            assert!(auth.code().is_none());
        }
    }
    #[test]
    fn accepted_token_uses_store_off_thread_and_reports_failure() {
        let mut auth = waiting();
        deliver(&mut auth, Ok(Reply::Token("fixture-token".into())));
        auth.poll_with_store(|token| {
            assert_eq!(
                token.map(ExposeSecret::expose_secret),
                Some("fixture-token")
            );
            assert_eq!(thread::current().name(), Some("herdr-github-auth"));
            Err(std::io::Error::other("mock Keychain locked").into())
        });
        auth.cancel(); // Accepted commits cannot be cancelled halfway through Keychain I/O.
        let reply = auth
            .incoming
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        deliver(&mut auth, reply);
        auth.poll();
        assert_eq!(auth.message.as_deref(), Some("mock Keychain locked"));
        assert!(!auth.busy());
    }
}
