use std::fmt;
use std::future::Future;
use std::time::Duration;

use serde::Serialize;
use tokio::time::Instant;

pub const REQUEST_BUDGET: Duration = Duration::from_secs(3);
pub const SECTION_BUDGET: Duration = Duration::from_secs(15);
pub const TOTAL_BUDGET: Duration = Duration::from_secs(70);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Fail,
    Unknown,
    Warn,
    Skipped,
    Pass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    Usage,
    Auth,
    Deadline,
    Failure,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Server,
    Account,
    Connections,
    Agent,
    Terminal,
    Project,
    Install,
    Worker,
    Cyber,
    Review,
}

impl Area {
    pub fn label(self) -> &'static str {
        match self {
            Self::Server => "Network",
            Self::Account => "Sign-in",
            Self::Connections => "Connections",
            Self::Agent => "Agent",
            Self::Terminal => "Terminal",
            Self::Project => "Project",
            Self::Install => "Install",
            Self::Worker => "Worker",
            Self::Cyber => "Cyber",
            Self::Review => "Review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    Run(String),
    Set { key: String, value: String },
    Step(String),
    Guide { command: String, text: String },
}

impl Fix {
    pub fn command(&self) -> Option<String> {
        match self {
            Self::Run(command) | Self::Guide { command, .. } => Some(command.clone()),
            Self::Set { key, value } => Some(format!("{key}={value}")),
            Self::Step(step) => step.split('`').nth(1).map(str::to_string),
        }
    }
}

impl fmt::Display for Fix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(command) => write!(formatter, "Run `{command}`."),
            Self::Set { key, value } => write!(formatter, "Set {key} to {value}."),
            Self::Step(text) | Self::Guide { text, .. } => formatter.write_str(text),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    Usage,
    Auth,
    Deadline,
    Interrupted,
}

#[derive(Debug, Clone)]
pub struct Check<E = ()> {
    pub id: &'static str,
    pub label: &'static str,
    pub area: Area,
    pub status: Status,
    pub cause: Option<Cause>,
    pub summary: String,
    pub detail: Option<String>,
    pub subject: Option<String>,
    pub blocked_by: Option<&'static str>,
    pub fix: Option<Fix>,
    pub evidence: E,
}

impl<E: Default> Check<E> {
    pub fn new(area: Area, id: &'static str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            id,
            label: id,
            area,
            status,
            cause: None,
            summary: summary.into(),
            detail: None,
            subject: None,
            blocked_by: None,
            fix: None,
            evidence: E::default(),
        }
    }
}

impl<E> Check<E> {
    pub fn label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }

    pub fn cause(mut self, cause: Cause) -> Self {
        self.cause = Some(cause);
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    pub fn blocked_by(mut self, id: &'static str) -> Self {
        self.blocked_by = Some(id);
        self
    }

    pub fn fix(mut self, fix: Fix) -> Self {
        self.fix = Some(fix);
        self
    }

    pub fn evidence(mut self, evidence: E) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn fix_text(&self) -> Option<String> {
        self.fix.as_ref().map(Fix::to_string)
    }
}

#[derive(Debug, Clone)]
pub struct Report<E = ()> {
    pub title: &'static str,
    pub version: String,
    pub server: Option<String>,
    pub tally: Option<String>,
    pub checks: Vec<Check<E>>,
}

impl<E> Report<E> {
    pub fn new(title: &'static str, version: &str) -> Self {
        Self {
            title,
            version: version.to_string(),
            server: None,
            tally: None,
            checks: Vec::new(),
        }
    }

    pub fn push(&mut self, check: Check<E>) {
        self.checks.push(check);
    }

    pub fn overall(&self) -> Status {
        overall(&self.checks)
    }

    pub fn outcome(&self) -> Outcome {
        let has = |cause| self.checks.iter().any(|check| check.cause == Some(cause));
        if has(Cause::Interrupted) {
            Outcome::Interrupted
        } else if has(Cause::Usage) {
            Outcome::Usage
        } else if has(Cause::Auth) {
            Outcome::Auth
        } else if has(Cause::Deadline) {
            Outcome::Deadline
        } else if has(Cause::Failure)
            || self.checks.iter().any(|check| check.status == Status::Fail)
        {
            Outcome::Failed
        } else {
            Outcome::Ok
        }
    }

    pub fn map_text(&mut self, clean: impl Fn(&str) -> String) {
        for check in &mut self.checks {
            check.summary = clean(&check.summary);
            check.detail = check.detail.as_deref().map(&clean);
            check.subject = check.subject.as_deref().map(&clean);
            check.fix = check.fix.take().map(|fix| match fix {
                Fix::Run(command) => Fix::Run(clean(&command)),
                Fix::Set { key, value } => Fix::Set {
                    key: clean(&key),
                    value: clean(&value),
                },
                Fix::Step(step) => Fix::Step(clean(&step)),
                Fix::Guide { command, text } => Fix::Guide {
                    command: clean(&command),
                    text: clean(&text),
                },
            });
        }
        self.version = clean(&self.version);
        self.server = self.server.as_deref().map(&clean);
        self.tally = self.tally.as_deref().map(&clean);
    }
}

pub fn overall<E>(checks: &[Check<E>]) -> Status {
    [Status::Fail, Status::Unknown, Status::Warn]
        .into_iter()
        .find(|status| checks.iter().any(|check| check.status == *status))
        .unwrap_or(Status::Pass)
}

#[derive(Debug, Clone, Copy)]
pub struct Runner {
    deadline: Instant,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new(TOTAL_BUDGET)
    }
}

impl Runner {
    pub fn new(budget: Duration) -> Self {
        Self {
            deadline: Instant::now() + budget,
        }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn section(&self, budget: Duration) -> Self {
        Self {
            deadline: self.deadline.min(Instant::now() + budget),
        }
    }

    pub async fn within<T>(&self, work: impl Future<Output = T>) -> Result<T, Cause> {
        tokio::select! {
            result = tokio::time::timeout_at(self.deadline, work) => result.map_err(|_| Cause::Deadline),
            signal = tokio::signal::ctrl_c() => Err(match signal {
                Ok(()) => Cause::Interrupted,
                Err(_) => Cause::Failure,
            }),
        }
    }

    pub async fn request<T>(&self, work: impl Future<Output = T>) -> Result<T, Cause> {
        self.section(REQUEST_BUDGET).within(work).await
    }
}
