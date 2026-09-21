//! GitHub's device authorization flow: the codes it hands back, the replies it
//! can give, and the verified profile a completed flow yields. Secrets are
//! deserialized straight into redacted types and never pass through a String.

use super::{
    Result,
    http::{agent, authorization, response},
    valid_token,
};
use crate::Error;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const VERIFY_URL: &str = "https://github.com/login/device";
pub(super) const SETUP_MESSAGE: &str = "Connect with Herdr GPUI's GitHub App. Optionally set [github] oauth_client_id in config-gpui.toml, or HERDR_GITHUB_OAUTH_CLIENT_ID, to another GitHub App or OAuth App public client ID with Device Flow enabled. Reload GUI config after file edits.";

#[derive(Debug, Deserialize)]
pub(super) struct Device {
    pub(super) device_code: SecretString,
    pub(super) user_code: SecretString,
    pub(super) verification_uri: String,
    pub(super) expires_in: u64,
    #[serde(default = "default_interval")]
    pub(super) interval: u64,
}
pub(super) fn default_interval() -> u64 {
    5
}

impl Device {
    pub(super) fn validate(self) -> Result<Self> {
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

pub(super) enum Reply {
    Device(Device, String, Instant),
    Pending(bool),
    Token(SecretString),
    SignedOut,
    Authenticated(Arc<SecretString>),
}

pub(crate) struct Profile {
    pub login: String,
    pub avatar: Option<Arc<gpui::Image>>,
    pub token: Arc<SecretString>,
    pub(super) avatar_updates: Option<crate::avatars::AvatarUpdates>,
}

pub(super) fn profile(token: Arc<SecretString>) -> Result<Profile> {
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
pub(super) struct TokenResponse {
    pub(super) access_token: Option<SecretString>,
    pub(super) token_type: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) fn token_reply(value: TokenResponse) -> Result<Reply> {
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
