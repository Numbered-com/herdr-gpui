//! Native GitHub transport and device authorization. No credential subprocesses.

mod auth;
mod credentials;
mod device;
mod http;
mod store;

#[cfg(test)]
mod tests;

pub(crate) use {
    auth::Auth,
    device::{Profile, VERIFY_URL},
    http::graphql,
    store::{Note, Store},
};

use crate::Result;
use device::{Device, Reply, SETUP_MESSAGE, profile, token_reply};
use store::{load_token, save, valid_token};
