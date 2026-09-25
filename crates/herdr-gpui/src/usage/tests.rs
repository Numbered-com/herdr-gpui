#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(unix)]
use super::remote;
use super::{
    Host, Reading, Usage,
    fetch::{claude_credential, codex_credential},
    model::{Kind, Provider, Report, Severity, Window, countdown},
    parse::{Raw, report},
};
use crate::Error;
use secrecy::ExposeSecret;
use std::time::{Duration, Instant, SystemTime};

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn raw(provider: Provider, status: u16, body: &str) -> Raw {
    Raw {
        provider,
        status,
        body: body.into(),
    }
}

/// Trimmed from a live response: model windows only appear in `limits`.
const CLAUDE: &str = r#"{"five_hour":{"utilization":3.0,"resets_at":"2026-09-25T08:20:00.978246+00:00"},
"seven_day":{"utilization":15.0,"resets_at":"2026-09-29T16:59:59+00:00"},
"extra_usage":{"is_enabled":false},
"limits":[
 {"kind":"session","group":"session","percent":3,"resets_at":"2026-09-25T08:20:00+00:00","scope":null},
 {"kind":"weekly_all","group":"weekly","percent":15,"resets_at":"2026-09-29T16:59:59+00:00","scope":null},
 {"kind":"weekly_scoped","group":"weekly","percent":0,"resets_at":"2026-09-29T17:00:00+00:00",
  "scope":{"model":{"id":null,"display_name":"Fable"},"surface":null}},
 {"kind":"monthly_spend","percent":50,"resets_at":null}
]}"#;

#[test]
fn claude_limits_name_every_window_in_display_order() {
    let report = report(&raw(Provider::Claude, 200, CLAUDE)).unwrap();
    let windows: Vec<_> = report
        .windows
        .iter()
        .map(|w| (w.kind.clone(), w.percent()))
        .collect();
    assert_eq!(
        windows,
        [
            (Kind::Session, 3),
            (Kind::Weekly, 15),
            (Kind::Model("Fable".into()), 0)
        ]
    );
    assert_eq!(report.windows[0].resets_at, Some(at(1_790_324_400)));
    assert_eq!(report.tightest().map(|w| w.percent()), Some(15));
}

#[test]
fn claude_falls_back_to_the_fixed_windows() {
    let body = r#"{"five_hour":{"utilization":42.4,"resets_at":1790324400},
        "seven_day":{"utilization":150,"resets_at":null},"limits":null}"#;
    let report = report(&raw(Provider::Claude, 200, body)).unwrap();
    assert_eq!(report.windows[0].kind, Kind::Session);
    assert_eq!(report.windows[0].percent(), 42);
    assert_eq!(report.windows[0].resets_at, Some(at(1_790_324_400)));
    // Clamped: a service rounding past its own limit still reads as full.
    assert_eq!(report.windows[1].percent(), 100);
    assert_eq!(report.windows[1].resets_at, None);
}

#[test]
fn codex_windows_are_known_by_length_not_slot() {
    let parse = |body| report(&raw(Provider::Codex, 200, body)).unwrap();
    // Live shape: a Pro plan with only a weekly limit, in the primary slot.
    let body = r#"{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":11,
        "limit_window_seconds":604800,"reset_after_seconds":472393,"reset_at":1790786634},
        "secondary_window":null},"credits":{"balance":"0"}}"#;
    let report = parse(body);
    assert_eq!(report.plan.as_deref(), Some("Pro"));
    assert_eq!(report.windows.len(), 1);
    assert_eq!(report.windows[0].kind, Kind::Weekly);
    assert_eq!(report.windows[0].resets_at, Some(at(1_790_786_634)));

    let body = r#"{"plan_type":"plus","rate_limit":{
        "primary_window":{"used_percent":70,"limit_window_seconds":1,"reset_at":1790000000000},
        "secondary_window":{"used_percent":90,"reset_at":null}}}"#;
    let report = parse(body);
    assert_eq!(report.windows[0].kind, Kind::Session);
    // Milliseconds are recognised as such.
    assert_eq!(report.windows[0].resets_at, Some(at(1_790_000_000)));
    assert_eq!(report.windows[1].kind, Kind::Weekly);
    assert_eq!(report.tightest().map(|w| w.percent()), Some(90));
}

#[test]
fn statuses_become_typed_errors_without_echoing_the_body() {
    for (status, expected) in [
        (0, "Connect"),
        (401, "Rejected"),
        (403, "Rejected"),
        (429, "RateLimited"),
        (500, "Status"),
    ] {
        let error = report(&raw(Provider::Claude, status, "{}")).unwrap_err();
        let kind = match error {
            Error::UsageConnect => "Connect",
            Error::UsageRejected => "Rejected",
            Error::UsageRateLimited => "RateLimited",
            Error::UsageStatus(500) => "Status",
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(kind, expected, "status {status}");
    }
    let error = report(&raw(
        Provider::Codex,
        200,
        r#"{"plan_type": "secret@example.com"#,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        Error::UsageJson(serde_json::error::Category::Eof)
    ));
    assert!(!error.to_string().contains("secret"));
    let error = report(&raw(
        Provider::Codex,
        200,
        r#"{"rate_limit":{"primary_window":{"used_percent":"x@y"}}}"#,
    ))
    .unwrap_err();
    assert!(!error.to_string().contains("x@y"));
}

#[test]
fn labels_count_down_in_the_two_coarsest_units() {
    assert_eq!(countdown(Duration::ZERO), "0m");
    assert_eq!(countdown(Duration::from_secs(47 * 60 - 30)), "47m");
    assert_eq!(countdown(Duration::from_secs(2 * 3600 + 53 * 60)), "2h 53m");
    assert_eq!(
        countdown(Duration::from_secs(4 * 86_400 + 11 * 3600 + 59)),
        "4d 11h"
    );
    let now = at(1_000_000);
    let session = Window::new(Kind::Session, 2.4, Some(now + Duration::from_secs(10_380)));
    assert_eq!(session.label(now), "2% used 2h 53m");
    assert_eq!(session.detail(now), "Session 2% used · resets in 2h 53m");
    // A reset already past reads as imminent rather than negative.
    assert_eq!(
        session.label(now + Duration::from_secs(20_000)),
        "2% used 0m"
    );
    let model = Window::new(Kind::Model("Fable".into()), 0., Some(now));
    assert_eq!(model.label(now), "0% used Fable");
    assert_eq!(
        Window::new(Kind::Session, 1., None).label(now),
        "1% used 5h"
    );
    assert_eq!(Window::new(Kind::Weekly, 1., None).label(now), "1% used wk");
}

#[test]
fn severity_thresholds() {
    assert_eq!(Severity::from(59.9), Severity::Normal);
    assert_eq!(Severity::from(60.), Severity::Warning);
    assert_eq!(Severity::from(80.), Severity::Critical);
}

#[test]
fn hosts_follow_the_connection_target() {
    use herdr_client::ConnectTarget;
    assert_eq!(Host::from(&ConnectTarget::Local), Host::Local);
    assert_eq!(
        Host::from(&ConnectTarget::Socket("/tmp/x.sock".into())),
        Host::Local
    );
    assert_eq!(
        Host::from(&ConnectTarget::Ssh {
            target: "me@box".into(),
            session: "default".into()
        }),
        Host::Ssh("me@box".into())
    );
}

#[test]
fn credentials_are_read_only_when_they_hold_a_token() {
    let claude = claude_credential(
        br#"{"claudeAiOauth":{"accessToken":"fixture-token","refreshToken":"r","expiresAt":1}}"#,
    )
    .unwrap();
    assert_eq!(claude.expose_secret(), "fixture-token");
    assert!(claude_credential(br#"{"claudeAiOauth":{"accessToken":" "}}"#).is_none());
    assert!(claude_credential(br#"{"other":{}}"#).is_none());
    assert!(claude_credential(b"not json").is_none());

    assert!(codex_credential(
        br#"{"auth_mode":"chatgpt","tokens":{"access_token":"a","account_id":"acct","id_token":"i"}}"#
    )
    .is_some());
    // An API-key install has no plan usage.
    assert!(codex_credential(br#"{"OPENAI_API_KEY":"sk-fixture","tokens":null}"#).is_none());
}

#[cfg(unix)]
#[test]
fn script_output_splits_per_provider() {
    let output = "motd from a noisy rc file\n\
        \n@@herdr-usage claude\n{\"a\":1}\n@@herdr-status 200\n\
        \n@@herdr-usage mystery\n{}\n@@herdr-status 200\n\
        \n@@herdr-usage codex\n{\"b\":\"\n@@herdr-status 1\"}\n@@herdr-status 401\n";
    assert_eq!(
        remote::sections(output).unwrap(),
        [
            raw(Provider::Claude, 200, "{\"a\":1}"),
            raw(Provider::Codex, 401, "{\"b\":\"\n@@herdr-status 1\"}"),
        ]
    );
    // curl killed before writing its status: no answer.
    assert_eq!(
        remote::sections("\n@@herdr-usage codex\n{\"partial\"").unwrap(),
        [raw(Provider::Codex, 0, "{\"partial\"")]
    );
    assert!(remote::sections("").unwrap().is_empty());
    assert!(matches!(
        remote::sections("@@herdr-usage-missing-curl\n"),
        Err(Error::UsageMissingCurl)
    ));
}

#[cfg(unix)]
#[test]
fn script_repeats_the_shared_endpoints() {
    for value in remote::SHARED_WITH_SCRIPT {
        assert!(remote::SCRIPT_FOR_TESTS.contains(value), "{value}");
    }
}

/// Runs the remote script against a fake home and a fake `curl`, checking that
/// tokens reach curl on stdin and never in its arguments.
#[cfg(unix)]
#[test]
fn script_sends_tokens_on_stdin_only() {
    use std::{os::unix::fs::PermissionsExt, process::Command};
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = root.path().join("bin");
    let log = root.path().join("log");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        home.join(".claude/.credentials.json"),
        r#"{"claudeAiOauth":{"accessToken":"claude-fixture-token","refreshToken":"refresh-fixture"}}"#,
    )
    .unwrap();
    std::fs::write(
        home.join(".codex/auth.json"),
        "{\n  \"tokens\": {\n    \"id_token\": \"id-fixture\",\n    \"access_token\": \"codex-fixture-token\",\n    \"account_id\": \"acct-fixture\"\n  }\n}\n",
    )
    .unwrap();
    let curl = bin.join("curl");
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nprintf 'args: %s\\n' \"$*\" >> '{log}'\ncat >> '{log}'\nprintf '{{\"ok\":true}}\\n@@herdr-status 200\\n'\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new("/bin/sh")
        .args(["-c", remote::SCRIPT_FOR_TESTS])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(output.status.success());
    let raws = remote::sections(&String::from_utf8(output.stdout).unwrap()).unwrap();
    assert_eq!(
        raws,
        [
            raw(Provider::Claude, 200, "{\"ok\":true}"),
            raw(Provider::Codex, 200, "{\"ok\":true}"),
        ]
    );
    let log = std::fs::read_to_string(log).unwrap();
    for line in log.lines().filter(|line| line.starts_with("args: ")) {
        assert!(!line.contains("fixture"), "token in argv: {line}");
    }
    assert!(log.contains("Authorization: Bearer claude-fixture-token\n"));
    assert!(log.contains("Authorization: Bearer codex-fixture-token\n"));
    assert!(log.contains("ChatGPT-Account-Id: acct-fixture\n"));
    assert!(!log.contains("refresh-fixture") && !log.contains("id-fixture"));
}

#[cfg(unix)]
#[test]
fn script_reports_a_host_without_curl() {
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", remote::SCRIPT_FOR_TESTS])
        .env_clear()
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    assert!(matches!(
        remote::sections(&String::from_utf8(output.stdout).unwrap()),
        Err(Error::UsageMissingCurl)
    ));
}

fn claude_report(used: f64) -> Report {
    Report::new(
        Provider::Claude,
        None,
        vec![Window::new(Kind::Session, used, None)],
    )
}

#[test]
fn a_failed_refresh_keeps_the_last_numbers_and_says_why() {
    let now = Instant::now();
    let local = Host::Local;
    let mut usage = Usage::default();
    assert!(usage.poll(Some(local.clone()), false, now));
    assert!(
        usage.current().is_none(),
        "an inactive window reads nothing"
    );
    assert!(!usage.busy());

    usage.apply(
        local.clone(),
        Ok(vec![(Provider::Claude, Ok(claude_report(20.)))]),
        now,
    );
    usage.apply(
        local.clone(),
        Ok(vec![
            (Provider::Claude, Err(Error::UsageRateLimited)),
            (Provider::Codex, Err(Error::UsageRejected)),
        ]),
        now,
    );
    let entry = usage.current().unwrap();
    assert_eq!(
        entry.readings,
        [
            Reading {
                provider: Provider::Claude,
                report: Some(claude_report(20.)),
                error: Some(Error::UsageRateLimited.to_string()),
            },
            Reading {
                provider: Provider::Codex,
                report: None,
                error: Some(Error::UsageRejected.to_string()),
            },
        ]
    );
    assert_eq!(entry.due, Some(now + super::RATE_LIMITED));

    // The host becoming unreachable keeps every reading.
    usage.apply(local.clone(), Err(Error::UsageUnreachable), now);
    let entry = usage.current().unwrap();
    assert_eq!(entry.readings.len(), 2);
    assert_eq!(entry.error, Some(Error::UsageUnreachable.to_string()));
    assert_eq!(entry.due, Some(now + super::ERROR_BACKOFF));

    // Signing out of an agent drops it.
    usage.apply(local, Ok(vec![]), now);
    assert!(usage.current().unwrap().readings.is_empty());
}

#[test]
fn each_host_keeps_its_own_answer_within_a_bound() {
    let now = Instant::now();
    let mut usage = Usage::default();
    let remote = Host::Ssh("me@box".into());
    usage.apply(
        Host::Local,
        Ok(vec![(Provider::Claude, Ok(claude_report(1.)))]),
        now,
    );
    usage.apply(
        remote.clone(),
        Ok(vec![(Provider::Codex, Ok(claude_report(2.)))]),
        now,
    );
    usage.poll(Some(remote.clone()), false, now);
    assert_eq!(
        usage.current().unwrap().readings[0].provider,
        Provider::Codex
    );
    usage.poll(Some(Host::Local), false, now);
    assert_eq!(
        usage.current().unwrap().readings[0].provider,
        Provider::Claude
    );
    usage.poll(None, false, now);
    assert!(usage.current().is_none());

    usage.poll(Some(remote.clone()), false, now);
    for index in 0..super::HOST_LIMIT * 2 {
        usage.apply(Host::Ssh(format!("host-{index}")), Ok(vec![]), now);
    }
    assert!(usage.entries.len() <= super::HOST_LIMIT);
    assert!(
        usage.current().is_some(),
        "the shown host survives trimming"
    );
}

#[test]
fn manual_refresh_is_spaced() {
    let now = Instant::now();
    let mut usage = Usage::default();
    usage.poll(Some(Host::Local), false, now);
    usage.apply(Host::Local, Ok(vec![]), now);
    usage.entries.get_mut(&Host::Local).unwrap().requested = Some(now);
    usage.refresh(now + Duration::from_secs(1));
    assert_eq!(usage.current().unwrap().due, Some(now + super::REFRESH));
    usage.refresh(now + super::MANUAL_SPACING);
    assert_eq!(
        usage.current().unwrap().due,
        Some(now + super::MANUAL_SPACING)
    );
}
