//! Native GitHub transport and device authorization. No credential subprocesses.
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

const LIMIT: u64 = 2 * 1024 * 1024;
const SERVICE: &str = "dev.herdr.gpui.github";
const ACCOUNT: &str = "github.com";
pub(super) const VERIFY_URL: &str = "https://github.com/login/device";

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

fn response<T: DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<T, String> {
    match response.status().as_u16() {
        200..=299 => {},
        401 => return Err("GitHub authentication required. Open GitHub sign-in, or replace your environment token.".into()),
        403 => return Err("GitHub denied access: check token permissions, SSO authorization, or rate limits.".into()),
        429 => return Err("GitHub rate limit reached. Retry later.".into()),
        _ => return Err("GitHub request failed. Check network and repository access.".into()),
    }
    // Allocate the bounded capacity up front: no reallocations leave old body
    // fragments behind, and partial reads are wiped even on I/O errors.
    let mut bytes = Zeroizing::new(Vec::with_capacity(LIMIT as usize + 1));
    response
        .body_mut()
        .as_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "GitHub response could not be read within the size/time limit.")?;
    if bytes.len() > LIMIT as usize {
        return Err("GitHub response could not be read within the size/time limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid GitHub JSON response.".into())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|b| b.is_ascii_graphic())
}

fn resolve_token(
    gh: Option<SecretString>,
    github: Option<SecretString>,
    saved: impl FnOnce() -> Result<Option<SecretString>, String>,
) -> Result<SecretString, String> {
    let token = match gh
        .filter(|s| !s.expose_secret().trim().is_empty())
        .or_else(|| github.filter(|s| !s.expose_secret().trim().is_empty()))
    {
        Some(token) => Some(token),
        None => saved()?,
    }
    .ok_or(
        "GitHub authentication required. Use menu > GitHub sign-in or set GH_TOKEN / GITHUB_TOKEN.",
    )?;
    let trimmed = token.expose_secret().trim();
    if !valid_token(trimmed) {
        return Err("Invalid GitHub token. Replace the configured credential.".into());
    }
    if trimmed.len() == token.expose_secret().len() {
        Ok(token)
    } else {
        Ok(trimmed.into())
    }
}

fn credential_bytes(bytes: Vec<u8>) -> Result<SecretString, String> {
    let bytes = Zeroizing::new(bytes);
    // Validate by borrowing so invalid UTF-8 never escapes in FromUtf8Error.
    std::str::from_utf8(&bytes)
        .map(SecretString::from)
        .map_err(|_| "Invalid GitHub credential encoding.".into())
}

fn environment_token(name: &str) -> Result<Option<SecretString>, String> {
    // Own and wipe even non-Unicode environment values. The process environment
    // itself is outside this allocation's lifetime and is not erased here.
    std::env::var_os(name)
        .map(|value| credential_bytes(value.into_encoded_bytes()))
        .transpose()
}

#[cfg(target_os = "macos")]
fn saved_token() -> Result<Option<SecretString>, String> {
    match security_framework::passwords::get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => credential_bytes(bytes).map(Some),
        Err(e) if e.code() == -25300 => Ok(None),
        Err(_) => Err(
            "Cannot read GitHub Keychain entry. Unlock your login Keychain or set GH_TOKEN.".into(),
        ),
    }
}

#[cfg(not(target_os = "macos"))]
fn saved_token() -> Result<Option<SecretString>, String> {
    Ok(None)
}

fn store(token: Option<&SecretString>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use security_framework::passwords::{delete_generic_password, set_generic_password};
        let result = match token {
            Some(token) => set_generic_password(SERVICE, ACCOUNT, token.expose_secret().as_bytes()),
            None => delete_generic_password(SERVICE, ACCOUNT),
        };
        match result {
            Ok(()) => Ok(()),
            Err(e) if token.is_none() && e.code() == -25300 => Ok(()),
            Err(_) => Err(
                "GitHub Keychain update failed. Unlock your login Keychain and try again.".into(),
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (token, SERVICE, ACCOUNT);
        Err("Native token storage requires macOS. Use GH_TOKEN / GITHUB_TOKEN.".into())
    }
}

pub(super) fn graphql(
    query: &str,
    variables: Value,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    if cancelled() {
        return Err("PR lookup cancelled.".into());
    }
    let gh = environment_token("GH_TOKEN")?;
    let github = if gh
        .as_ref()
        .is_none_or(|s| s.expose_secret().trim().is_empty())
    {
        environment_token("GITHUB_TOKEN")?
    } else {
        None
    };
    let token = resolve_token(gh, github, saved_token)?;
    if cancelled() {
        return Err("PR lookup cancelled.".into());
    }
    let body = serde_json::json!({"query":query,"variables":variables}).to_string();
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or("PR lookup timed out (15 seconds).")?;
    let result: Value = response(
        agent(timeout)
            .post("https://api.github.com/graphql")
            .header("User-Agent", "Herdr-GPUI")
            .header("Accept", "application/vnd.github+json")
            .header("Content-Type", "application/json")
            .header("Authorization", authorization(&token)?)
            .send(body.as_bytes())
            .map_err(|_| "GitHub network request failed or timed out.")?,
    )?;
    if cancelled() {
        return Err("PR lookup cancelled.".into());
    }
    if result.get("errors").is_some() {
        return Err(
            "GitHub query failed. Check token repository permissions and rate limits.".into(),
        );
    }
    Ok(result)
}

fn authorization(token: &SecretString) -> Result<ureq::http::HeaderValue, String> {
    let mut text = Secret::new(Box::new(String::with_capacity(
        7 + token.expose_secret().len(),
    )));
    text.expose_secret_mut().push_str("Bearer ");
    text.expose_secret_mut().push_str(token.expose_secret());
    let mut header = ureq::http::HeaderValue::from_str(text.expose_secret())
        .map_err(|_| "Invalid GitHub authorization header.")?;
    header.set_sensitive(true);
    Ok(header)
}

fn oauth<T: DeserializeOwned>(
    path: &str,
    fields: &[(&str, &str)],
    timeout: Duration,
) -> Result<T, String> {
    // ureq owns form serialization and HTTP/TLS buffers; their copies cannot be
    // zeroized by this module. Never log request fields or raw response errors.
    response(
        agent(timeout)
            .post(format!("https://github.com/login/{path}"))
            .header("User-Agent", "Herdr-GPUI")
            .header("Accept", "application/json")
            .send_form(fields.iter().copied())
            .map_err(|_| "GitHub sign-in request failed or timed out.")?,
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
    fn validate(self) -> Result<Self, String> {
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
            return Err("Invalid GitHub device authorization response.".into());
        }
        Ok(self)
    }
}

enum Reply {
    Device(Device, String, Instant),
    Pending(bool),
    Token(SecretString),
    Stored(bool),
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<SecretString>,
    token_type: Option<String>,
    error: Option<String>,
}

fn token_reply(value: TokenResponse) -> Result<Reply, String> {
    match value.error.as_deref() {
        Some("authorization_pending") => Ok(Reply::Pending(false)),
        Some("slow_down") => Ok(Reply::Pending(true)),
        Some("expired_token") => Err("GitHub code expired. Sign in again.".into()),
        Some("access_denied") => Err("GitHub authorization denied.".into()),
        Some(_) => Err("GitHub authorization failed. Check OAuth application settings.".into()),
        None => {
            let token = value
                .access_token
                .filter(|t| valid_token(t.expose_secret()))
                .ok_or("Invalid GitHub access token response.")?;
            if value
                .token_type
                .as_deref()
                .is_none_or(|t| !t.eq_ignore_ascii_case("bearer"))
            {
                return Err("Unsupported GitHub token type.".into());
            }
            Ok(Reply::Token(token))
        }
    }
}

struct Flow {
    client: String,
    device: Arc<Device>,
    interval: u64,
    deadline: Instant,
    next: Instant,
}

#[derive(Default)]
pub(super) struct Auth {
    flow: Option<Flow>,
    incoming: Option<mpsc::Receiver<Result<Reply, String>>>,
    cancelled: bool,
    committing: bool,
    pub message: Option<String>,
}

impl Auth {
    #[cfg(any(test, feature = "integration-test"))]
    pub(super) fn fixture(waiting: bool) -> Self {
        let now = Instant::now();
        Self {
            flow: waiting.then(|| Flow {
                client: "fixture".into(),
                device: Arc::new(Device { device_code: "fixture".into(), user_code: "ABCD-1234".into(), verification_uri: VERIFY_URL.into(), expires_in: 900, interval: 900 }),
                interval: 900,
                deadline: now + Duration::from_secs(900), next: now + Duration::from_secs(900),
            }),
            message: Some(if waiting { "Open GitHub and enter this code. Waiting for authorization..." } else { "Set HERDR_GITHUB_OAUTH_CLIENT_ID to your OAuth app client ID with Device Flow enabled, then relaunch. Alternatively set GH_TOKEN / GITHUB_TOKEN." }.into()),
            ..Self::default()
        }
    }
    pub fn busy(&self) -> bool {
        self.incoming.is_some() || self.flow.is_some()
    }
    pub fn code(&self) -> Option<&str> {
        // The device user code is intentionally displayed, unlike access tokens.
        self.flow
            .as_ref()
            .map(|f| f.device.user_code.expose_secret())
    }
    fn launch(&mut self, work: impl FnOnce() -> Result<Reply, String> + Send + 'static) {
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
                self.message = Some("Could not start GitHub authentication worker.".into());
            }
        }
    }
    pub fn start(&mut self) {
        if self.busy() {
            return;
        }
        if !cfg!(target_os = "macos") {
            self.message =
                Some("Native token storage requires macOS. Use GH_TOKEN / GITHUB_TOKEN.".into());
            return;
        }
        let Some(client) = std::env::var("HERDR_GITHUB_OAUTH_CLIENT_ID")
            .ok()
            .filter(|s| valid_token(s))
        else {
            self.message = Some("Set HERDR_GITHUB_OAUTH_CLIENT_ID to your OAuth app client ID with Device Flow enabled, then relaunch. Alternatively set GH_TOKEN / GITHUB_TOKEN.".into());
            return;
        };
        self.cancelled = false;
        self.message = Some("Requesting GitHub sign-in code...".into());
        self.launch(move || {
            let started = Instant::now();
            let device = oauth::<Device>(
                "device/code",
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
        if self.committing {
            return;
        }
        self.cancelled = true;
        self.flow = None;
        self.message = Some("GitHub sign-in cancelled.".into());
    }
    pub fn sign_out(&mut self) {
        if self.busy() {
            return;
        }
        self.cancelled = false;
        self.committing = true;
        self.message = Some("Removing saved GitHub token...".into());
        self.launch(|| {
            store(None)?;
            Ok(Reply::Stored(false))
        });
    }
    pub fn poll(&mut self) -> bool {
        self.poll_with_store(store)
    }
    fn poll_with_store(
        &mut self,
        persist: fn(Option<&SecretString>) -> Result<(), String>,
    ) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.incoming {
            let reply = match rx.try_recv() {
                Ok(reply) => Some(reply),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("GitHub authentication worker stopped.".into()))
                }
            };
            if let Some(reply) = reply {
                self.incoming = None;
                changed = true;
                if !self.cancelled {
                    match reply {
                        Ok(Reply::Device(device, client, started)) => {
                            let now = Instant::now();
                            self.flow = Some(Flow {
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
                                self.message = Some("Saving GitHub token to Keychain...".into());
                                self.launch(move || {
                                    persist(Some(&token))?;
                                    Ok(Reply::Stored(true))
                                });
                            } else {
                                self.message = Some("GitHub code expired. Sign in again.".into());
                            }
                            self.flow = None;
                        }
                        Ok(Reply::Stored(signed_in)) => {
                            self.committing = false;
                            self.message = Some(if signed_in { "Signed in. Refresh the workspace PR. Environment tokens still take priority." } else { "Saved token removed. Environment tokens remain active; unset them and relaunch to sign out fully. GitHub grants are not revoked." }.into());
                        }
                        Err(error) => {
                            self.flow = None;
                            self.committing = false;
                            self.message = Some(error);
                        }
                    }
                }
            }
        }
        if self.incoming.is_none()
            && let Some(flow) = &self.flow
        {
            let now = Instant::now();
            if now >= flow.deadline {
                self.flow = None;
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

    fn device() -> Device {
        serde_json::from_str::<Device>(r#"{"device_code":"fixture-device", "user_code":"ABCD-1234", "verification_uri":"https://github.com/login/device", "expires_in":900, "interval":5}"#).unwrap().validate().unwrap()
    }
    fn token_reply(value: Value) -> Result<Reply, String> {
        super::token_reply(serde_json::from_value(value).unwrap())
    }
    fn deliver(auth: &mut Auth, reply: Result<Reply, String>) {
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
                .contains("authentication required")
        );
        assert_eq!(
            resolve_token(None, None, || Err("locked".into())).unwrap_err(),
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
        assert_eq!(error, "Invalid GitHub credential encoding.");
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
                response::<TokenResponse>(reply(body)).unwrap_err(),
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
            assert!(error.contains(message));
            assert!(!error.contains("private-error-secret"));
        }
        assert_eq!(
            response::<Value>(reply(200, b"{\"ok\":true}".to_vec())).unwrap()["ok"],
            true
        );
        assert!(response::<Value>(reply(200, b"not-json".to_vec())).is_err());
        assert!(response::<Value>(reply(200, vec![b' '; LIMIT as usize + 1])).is_err());
        assert!(
            graphql("", Value::Null, Duration::from_secs(1), || true)
                .unwrap_err()
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
            Err("mock Keychain locked".into())
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
