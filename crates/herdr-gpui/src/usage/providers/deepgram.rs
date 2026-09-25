//! Deepgram usage, read from the Management API with an API key sent as
//! `Authorization: Token …`: the `api_key` setting (or `DEEPGRAM_API_KEY`
//! here), else `DEEPGRAM_API_KEY` on the probed host. Without a configured
//! project, usage is summed over every project the key can see, as CodexBar
//! does. Deepgram reports usage, not a balance or a limit, so the panel
//! shows facts only. Everything CodexBar reads is ported.

use crate::{
    Error, Result,
    usage::{
        model::{Account, Provider, Report, Section, group},
        probe::{Probe, Request, Secret},
        service::{Service, Setting, json},
    },
};
use serde::Deserialize;

const BASE: &str = "https://api.deepgram.com/v1";
/// A key that sees many projects costs one request each; this bounds a refresh.
const MAX_PROJECTS: usize = 20;

pub(crate) struct Deepgram;

impl Service for Deepgram {
    fn id(&self) -> &'static str {
        "deepgram"
    }

    fn name(&self) -> &'static str {
        "Deepgram"
    }

    fn icon(&self) -> &'static str {
        "icons/providers/deepgram.svg"
    }

    fn dashboard(&self) -> Option<&'static str> {
        Some("https://console.deepgram.com/project/")
    }

    fn settings(&self) -> &'static [Setting] {
        const SETTINGS: &[Setting] = &[
            Setting::new(
                "api_key",
                &["DEEPGRAM_API_KEY"],
                "A Deepgram API key with the usage:read scope, created in the Deepgram Console \
                 (https://console.deepgram.com) under API Keys.",
            ),
            Setting::new(
                "project_id",
                &["DEEPGRAM_PROJECT_ID"],
                "Optional project UUID from the Deepgram Console. Leave unset to sum usage over \
                 every project the key can see.",
            ),
            Setting::new(
                "base_url",
                &["DEEPGRAM_API_URL"],
                "Optional HTTPS API URL for a proxy. Defaults to https://api.deepgram.com/v1.",
            ),
        ];
        SETTINGS
    }

    fn fetch(&self, probe: &mut Probe) -> Option<Result<Report>> {
        let key = probe
            .setting("api_key")
            .or_else(|| probe.env("DEEPGRAM_API_KEY"))?;
        Some(fetch(probe, &key))
    }
}

fn fetch(probe: &mut Probe, key: &Secret) -> Result<Report> {
    let base = base_url(probe.text_setting("base_url"))?;
    let get = |url: String| Request::get(url).secret_header("Authorization", "Token ", key);
    let projects = match probe.text_setting("project_id").filter(|id| !id.is_empty()) {
        Some(id) => vec![Project {
            project_id: id,
            name: None,
        }],
        None => parse_projects(&probe.http(get(format!("{base}/projects")))?.ok()?)?,
    };
    let mut totals = Totals::default();
    for project in projects.iter().take(MAX_PROJECTS) {
        let id: String =
            url::form_urlencoded::byte_serialize(project.project_id.as_bytes()).collect();
        let body = probe
            .http(get(format!("{base}/projects/{id}/usage/breakdown")))?
            .ok()?;
        totals.add(parse_breakdown(&body)?);
    }
    Ok(report(&projects, &totals))
}

/// An override must stay on HTTPS: the key is attached to it.
fn base_url(raw: Option<String>) -> Result<String> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok(BASE.to_owned());
    };
    let url = if raw.contains("://") {
        raw
    } else {
        format!("https://{raw}")
    };
    let parsed = url::Url::parse(&url).map_err(|_| Error::UsageNotSignedIn)?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Error::UsageNotSignedIn);
    }
    Ok(url.trim_end_matches('/').to_owned())
}

fn parse_projects(body: &str) -> Result<Vec<Project>> {
    Ok(json::<Projects>(body)?.projects)
}

fn parse_breakdown(body: &str) -> Result<Totals> {
    let breakdown: Breakdown = json(body)?;
    let mut totals = Totals {
        start: breakdown.start,
        end: breakdown.end,
        ..Totals::default()
    };
    for row in breakdown.results {
        totals.hours += row.hours.unwrap_or(0.);
        totals.total_hours += row.total_hours.unwrap_or(0.);
        totals.agent_hours += row.agent_hours.unwrap_or(0.);
        totals.tokens_in += row.tokens_in.unwrap_or(0.);
        totals.tokens_out += row.tokens_out.unwrap_or(0.);
        totals.tts += row.tts_characters.unwrap_or(0.);
        totals.requests += row.requests.unwrap_or(0.);
    }
    Ok(totals)
}

fn report(projects: &[Project], totals: &Totals) -> Report {
    let plan = match projects {
        [] => "No projects".to_owned(),
        [project] => format!(
            "Project: {}",
            project.name.as_deref().unwrap_or(&project.project_id)
        ),
        projects => format!("{} projects", projects.len()),
    };
    let mut facts = vec![("Requests".to_owned(), whole(totals.requests))];
    if totals.hours > 0. || totals.total_hours > 0. {
        facts.push((
            "Audio".into(),
            format!(
                "{} hours · {} billable",
                decimal(totals.hours),
                decimal(totals.total_hours)
            ),
        ));
    }
    if totals.agent_hours > 0. {
        facts.push(("Agent hours".into(), decimal(totals.agent_hours)));
    }
    if totals.tokens_in > 0. || totals.tokens_out > 0. {
        facts.push(("Tokens".into(), whole(totals.tokens_in + totals.tokens_out)));
    }
    if totals.tts > 0. {
        facts.push(("TTS characters".into(), whole(totals.tts)));
    }
    if let (Some(start), Some(end)) = (&totals.start, &totals.end) {
        facts.push(("Period".into(), format!("{start} to {end}")));
    }
    Report::new(
        Provider(&Deepgram),
        Account {
            email: None,
            plan: Some(plan),
        },
        Vec::new(),
    )
    .with_sections([Section::Facts {
        title: "Usage summary".into(),
        facts,
    }])
}

fn whole(value: f64) -> String {
    group(value.round() as i64)
}

fn decimal(value: f64) -> String {
    if value.fract() == 0. {
        whole(value)
    } else {
        format!("{value:.1}")
    }
}

#[derive(Deserialize)]
struct Projects {
    projects: Vec<Project>,
}

#[derive(Deserialize)]
struct Project {
    project_id: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct Breakdown {
    start: Option<String>,
    end: Option<String>,
    results: Vec<Row>,
}

#[derive(Deserialize)]
struct Row {
    hours: Option<f64>,
    total_hours: Option<f64>,
    agent_hours: Option<f64>,
    tokens_in: Option<f64>,
    tokens_out: Option<f64>,
    tts_characters: Option<f64>,
    requests: Option<f64>,
}

#[derive(Default)]
struct Totals {
    start: Option<String>,
    end: Option<String>,
    hours: f64,
    total_hours: f64,
    agent_hours: f64,
    tokens_in: f64,
    tokens_out: f64,
    tts: f64,
    requests: f64,
}

impl Totals {
    /// Sums another project's usage, widening the period to cover both.
    fn add(&mut self, other: Self) {
        self.hours += other.hours;
        self.total_hours += other.total_hours;
        self.agent_hours += other.agent_hours;
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.tts += other.tts;
        self.requests += other.requests;
        if let Some(start) = other.start
            && self.start.as_ref().is_none_or(|current| start < *current)
        {
            self.start = Some(start);
        }
        if let Some(end) = other.end
            && self.end.as_ref().is_none_or(|current| end > *current)
        {
            self.end = Some(end);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const PROJECTS: &str = r#"{"projects":[
        {"project_id":"3f1c2a9e-1111-4b8e-9d7e-000000000001","name":"Voice bot"},
        {"project_id":"3f1c2a9e-1111-4b8e-9d7e-000000000002","name":null}
    ]}"#;

    const BREAKDOWN: &str = r#"{
        "start":"2026-06-01","end":"2026-06-30",
        "resolution":{"units":"day","amount":1},
        "results":[
            {"start":"2026-06-01","end":"2026-06-15","hours":1.5,"total_hours":2,
             "agent_hours":0.5,"tokens_in":1000,"tokens_out":250,"tts_characters":1200,"requests":40},
            {"start":"2026-06-16","end":"2026-06-30","hours":0.3,"total_hours":0.5,
             "requests":10,"tokens_in":null}
        ]
    }"#;

    #[test]
    fn sums_usage_across_projects() {
        let projects = parse_projects(PROJECTS).unwrap();
        assert_eq!(projects.len(), 2);
        let mut totals = Totals::default();
        totals.add(parse_breakdown(BREAKDOWN).unwrap());
        totals
            .add(parse_breakdown(r#"{"start":"2026-05-20","results":[{"requests":5}]}"#).unwrap());
        let report = report(&projects, &totals);
        assert!(report.windows.is_empty());
        assert_eq!(report.account.plan.as_deref(), Some("2 projects"));
        let Section::Facts { facts, .. } = &report.sections[0] else {
            panic!("expected facts");
        };
        let fact = |label: &str| {
            facts
                .iter()
                .find(|(name, _)| name == label)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(fact("Requests"), Some("55"));
        assert_eq!(fact("Audio"), Some("1.8 hours · 2.5 billable"));
        assert_eq!(fact("Tokens"), Some("1,250"));
        assert_eq!(fact("TTS characters"), Some("1,200"));
        assert_eq!(fact("Period"), Some("2026-05-20 to 2026-06-30"));
    }

    #[test]
    fn names_a_single_project() {
        let projects = parse_projects(PROJECTS).unwrap();
        let report = report(&projects[..1], &parse_breakdown(BREAKDOWN).unwrap());
        assert_eq!(report.account.plan.as_deref(), Some("Project: Voice bot"));
    }

    #[test]
    fn rejects_plain_http_overrides() {
        assert!(base_url(Some("http://proxy.test/v1".into())).is_err());
        assert_eq!(
            base_url(Some("proxy.test/v1/".into())).unwrap(),
            "https://proxy.test/v1"
        );
        assert_eq!(base_url(None).unwrap(), BASE);
    }

    #[test]
    fn rejects_a_breakdown_without_results() {
        assert!(parse_breakdown(r#"{"start":"2026-06-01"}"#).is_err());
    }
}
