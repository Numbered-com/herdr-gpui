//! Service answers to reports. Both local HTTP and the remote script hand over
//! the same status, body, and sign-in facts, so every host is judged alike.

use super::{
    model::{Provider, Report},
    service::Meta,
};
use crate::{Error, Result};

/// One provider's answer, before it is trusted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Raw {
    pub provider: Provider,
    /// The HTTP status, or 0 when the request never got an answer.
    pub status: u16,
    pub body: String,
    pub meta: Meta,
}

pub(crate) fn report(raw: &Raw) -> Result<Report> {
    match raw.status {
        200..=299 => raw.provider.service().parse(&raw.body, &raw.meta),
        0 => Err(Error::UsageConnect),
        401 | 403 => Err(Error::UsageRejected),
        429 => Err(Error::UsageRateLimited),
        status => Err(Error::UsageStatus(status)),
    }
}
