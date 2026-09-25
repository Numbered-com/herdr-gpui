//! Reading a host's plan usage. Locally, each agent's own saved sign-in is
//! read and sent straight to its service. A remote host runs one script over
//! SSH that does the same with its own sign-ins and prints only the service
//! responses, so no credential ever leaves the machine it belongs to.

#[cfg(unix)]
use super::remote;
use super::{
    model::{Host, Provider},
    parse::Raw,
    service::Request,
};
use crate::{Error, Result};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const LIMIT: usize = 256 * 1024;

/// Every provider this host is signed in to, with its service's answer. A
/// provider without a sign-in is absent rather than an error.
pub(super) fn fetch(host: &Host) -> Result<Vec<(Provider, Result<Raw>)>> {
    match host {
        Host::Local => Ok(Provider::ALL
            .into_iter()
            .filter_map(|provider| {
                let request = provider.service().local()?;
                Some((provider, request.and_then(|request| get(provider, request))))
            })
            .collect()),
        Host::Ssh(target) => Ok(remote::fetch(target)?
            .into_iter()
            .map(|raw| (raw.provider, Ok(raw)))
            .collect()),
    }
}

fn get(provider: Provider, request: Request) -> Result<Raw> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut call = agent.get(request.url);
    for (name, value) in request.headers {
        call = call.header(name, value);
    }
    let mut response = call.call().map_err(Error::UsageNetwork)?;
    let status = response.status().as_u16();
    let mut body = String::new();
    response
        .body_mut()
        .as_reader()
        .take(LIMIT as u64 + 1)
        .read_to_string(&mut body)
        .map_err(|source| Error::UsageNetwork(ureq::Error::Io(source)))?;
    if body.len() > LIMIT {
        return Err(Error::UsageSize);
    }
    Ok(Raw {
        provider,
        status,
        body,
        meta: request.meta,
    })
}

/// Runs `command` to completion with a deadline, keeping at most `LIMIT`
/// bytes of its standard output. Standard error is discarded: it may echo
/// what the child was reading.
pub(super) fn output(
    command: &mut Command,
    timeout: Duration,
    operation: &'static str,
) -> Result<(bool, Zeroizing<Vec<u8>>)> {
    let process = |source| Error::UsageProcess { operation, source };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(process)?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(process(std::io::Error::other("no output pipe")));
    };
    let (sender, reads) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("herdr-usage-output".into())
        .spawn(move || {
            let mut bytes = Zeroizing::new(Vec::new());
            let result = stdout
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes);
            let _ = sender.send(result);
        });
    if let Err(source) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return Err(process(source));
    }
    let result = match reads.recv_timeout(timeout) {
        Ok(Ok(bytes)) if bytes.len() > LIMIT => Err(Error::UsageSize),
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(source)) => Err(process(source)),
        Err(_) => Err(Error::UsageTimeout),
    };
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            // Killing the child closes the pipe, which ends the reader.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let status = child.wait().map_err(process)?;
    Ok((status.success(), bytes))
}

#[cfg(windows)]
mod remote {
    pub(super) fn fetch(_target: &str) -> crate::Result<Vec<super::Raw>> {
        Err(crate::Error::UsageUnsupported)
    }
}
