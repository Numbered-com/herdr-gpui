//! Plan usage in the terms the status bar shows: which agent, which limit
//! window, how much of it is used, and when it resets.

use crate::icons::AgentIcon;
use herdr_client::ConnectTarget;
use std::time::{Duration, SystemTime};

/// The machine whose agent sign-ins are read. A remote host is asked over SSH,
/// so its own credentials are used and never leave it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Host {
    Local,
    Ssh(String),
}

impl From<&ConnectTarget> for Host {
    fn from(target: &ConnectTarget) -> Self {
        match target {
            ConnectTarget::Ssh { target, .. } => Self::Ssh(target.clone()),
            ConnectTarget::Local | ConnectTarget::Session { .. } | ConnectTarget::Socket(_) => {
                Self::Local
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// The marker the remote script prints before this provider's response.
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn icon(self) -> AgentIcon {
        match self {
            Self::Claude => AgentIcon::Claude,
            Self::Codex => AgentIcon::Codex,
        }
    }

    #[cfg(unix)]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|provider| provider.key() == key)
    }
}

/// Which limit a window measures. Model windows are weekly limits scoped to
/// one model, named as the service names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Session,
    Weekly,
    Model(String),
}

impl Kind {
    pub fn title(&self) -> &str {
        match self {
            Self::Session => "Session",
            Self::Weekly => "Weekly",
            Self::Model(name) => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Window {
    pub kind: Kind,
    /// Percent of the window used, clamped to 0..=100.
    pub used: f32,
    pub resets_at: Option<SystemTime>,
}

impl Window {
    pub fn new(kind: Kind, used: f64, resets_at: Option<SystemTime>) -> Self {
        Self {
            kind,
            used: used.clamp(0., 100.) as f32,
            resets_at,
        }
    }

    pub fn percent(&self) -> u32 {
        self.used.round() as u32
    }

    /// `3% used 2h 53m`: the time left names a session or weekly window, and
    /// a model window is named instead, as the service does.
    pub fn label(&self, now: SystemTime) -> String {
        let suffix = match (&self.kind, self.resets_at) {
            (Kind::Model(name), _) => name.clone(),
            (_, Some(resets_at)) => countdown(resets_at.duration_since(now).unwrap_or_default()),
            (Kind::Session, None) => "5h".into(),
            (Kind::Weekly, None) => "wk".into(),
        };
        format!("{}% used {suffix}", self.percent())
    }

    /// `Weekly 15% used · resets in 4d 11h`, for the detail tooltip.
    pub fn detail(&self, now: SystemTime) -> String {
        let reset = self
            .resets_at
            .map(|at| {
                format!(
                    " · resets in {}",
                    countdown(at.duration_since(now).unwrap_or_default())
                )
            })
            .unwrap_or_default();
        format!("{} {}% used{reset}", self.kind.title(), self.percent())
    }
}

/// How alarming a window is, from the share of it already used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Normal,
    Warning,
    Critical,
}

impl From<f32> for Severity {
    fn from(used: f32) -> Self {
        if used >= 80. {
            Self::Critical
        } else if used >= 60. {
            Self::Warning
        } else {
            Self::Normal
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Report {
    pub provider: Provider,
    pub plan: Option<String>,
    /// Session, then weekly, then model windows.
    pub windows: Vec<Window>,
}

impl Report {
    pub fn new(provider: Provider, plan: Option<String>, mut windows: Vec<Window>) -> Self {
        windows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Self {
            provider,
            plan,
            windows,
        }
    }

    /// The window closest to its limit, which the meter shows.
    pub fn tightest(&self) -> Option<&Window> {
        self.windows.iter().max_by(|a, b| a.used.total_cmp(&b.used))
    }
}

/// `47m`, `2h 53m`, `4d 11h`: the coarsest two units that still say when.
pub(crate) fn countdown(left: Duration) -> String {
    let minutes = left.as_secs().div_ceil(60);
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}
