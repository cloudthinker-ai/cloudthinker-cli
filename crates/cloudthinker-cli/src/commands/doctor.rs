use std::path::{Path, PathBuf};

use cloudthinker_client::doctor::{Area, Cause, Check, Fix, Report, Runner, Status};
use cloudthinker_client::{
    CliConfig, OriginSource, ServerCheck, ServerEvidence, TOKEN_ENV_VAR, agent_bin_root,
    any_agent_installed, cli_config_path, config_dir, diagnose, installed_agent_binary,
};

use super::update::{self, ReleaseLookup};
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
const AGENT_DIR_ENV_VAR: &str = "CLOUDTHINKER_CODING_AGENT_DIR";

pub async fn run(
    base_url: &str,
    origin_source: OriginSource,
    workspace: Option<&str>,
    json: bool,
) -> ExitCode {
    let config = CliConfig::load_default();
    let runner = Runner::default();
    let step = (!json).then(|| output::step("Checking your CloudThinker setup..."));
    let mut diagnosis = diagnose(
        &runner,
        base_url,
        origin_source,
        config.saved_url(),
        workspace,
        CLI_VERSION,
    )
    .await;
    let home = dirs_home();
    let cwd = std::env::current_dir().ok();
    let local = [
        agent_check(),
        skills_check(home.as_deref(), cwd.as_deref()),
        release_check(&runner, base_url).await,
        terminal_check(),
        project_check(cwd.as_deref()),
        install_check(),
    ];
    diagnosis.report.checks.extend(local);
    drop(step);
    let code = exit::for_doctor(diagnosis.report.outcome());
    let report = std::mem::replace(&mut diagnosis.report, Report::new("doctor", CLI_VERSION));
    let printed = output::emit_doctor(report, json, |report| {
        diagnosis.report = report;
        diagnosis.cli_version = output::terminal_text(&diagnosis.cli_version);
        diagnosis.origin = diagnosis.origin.as_deref().map(output::terminal_text);
        diagnosis.into_wire(code as u8)
    });
    match printed {
        Ok(()) => code,
        Err(error) => exit::finish_output(Err(error)),
    }
}

fn local(
    area: Area,
    id: &'static str,
    label: &'static str,
    status: Status,
    summary: impl Into<String>,
) -> ServerCheck {
    Check::<ServerEvidence>::new(area, id, status, summary).label(label)
}

fn dirs_home() -> Option<PathBuf> {
    cli_config_path()
        .ok()
        .and_then(|path| path.parent().and_then(Path::parent).map(Path::to_path_buf))
}

fn agent_check() -> ServerCheck {
    let Ok(root) = agent_bin_root() else {
        return local(
            Area::Agent,
            "agent",
            "Agent",
            Status::Warn,
            "Home folder not found",
        );
    };
    if installed_agent_binary(&root, CLI_VERSION).is_some() {
        return local(
            Area::Agent,
            "agent",
            "Agent",
            Status::Pass,
            format!("Version {CLI_VERSION} installed"),
        );
    }
    let summary = if any_agent_installed(&root) {
        format!("Installed agent does not match CLI {CLI_VERSION}")
    } else {
        "Not installed yet".to_string()
    };
    local(Area::Agent, "agent", "Agent", Status::Warn, summary)
        .fix(Fix::Run("cloudthinker agent".into()))
}

async fn release_check(runner: &Runner, base_url: &str) -> ServerCheck {
    let channel = update::channel_of(base_url);
    let name = channel.name();
    let skipped = |summary: String| {
        local(
            Area::Install,
            "release",
            "Release",
            Status::Skipped,
            summary,
        )
    };
    match runner.request(update::lookup_latest(channel)).await {
        Ok(ReleaseLookup::Latest(Some(latest))) if update::is_newer(&latest) => local(
            Area::Install,
            "release",
            "Release",
            Status::Warn,
            format!("{latest} is available on the {name} channel"),
        )
        .fix(Fix::Run("cloudthinker update".into())),
        Ok(ReleaseLookup::Latest(_)) => local(
            Area::Install,
            "release",
            "Release",
            Status::Pass,
            format!("Up to date on the {name} channel"),
        ),
        Ok(ReleaseLookup::NoReceipt) => skipped(format!(
            "Not checked on the {name} channel: this copy was not installed by the installer"
        )),
        Ok(ReleaseLookup::Unreachable) | Err(Cause::Deadline | Cause::Failure) => skipped(format!(
            "Not checked on the {name} channel: the release server did not answer"
        )),
        Err(_) => skipped(format!("Not checked on the {name} channel: interrupted")),
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn terminal_check() -> ServerCheck {
    let term = env("TERM").unwrap_or_default();
    let program = env("TERM_PROGRAM").unwrap_or_default();
    let colour = if env("NO_COLOR").is_some() {
        "no colour (NO_COLOR)"
    } else if term == "dumb" {
        "no colour"
    } else if matches!(env("COLORTERM").as_deref(), Some("truecolor" | "24bit")) {
        "true colour"
    } else if term.contains("256color") {
        "256 colours"
    } else {
        "basic colour"
    };
    let remote = ["SSH_CONNECTION", "SSH_CLIENT", "MOSH_CONNECTION"]
        .into_iter()
        .any(|name| env(name).is_some());
    let headless = cfg!(target_os = "linux")
        && [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "TERMUX_VERSION",
            "WSL_DISTRO_NAME",
        ]
        .into_iter()
        .all(|name| env(name).is_none());
    let clipboard = if remote || headless {
        "clipboard via OSC 52"
    } else {
        "system clipboard"
    };
    let kitty = term == "xterm-kitty" || env("KITTY_WINDOW_ID").is_some() || program == "kitty";
    let notifications = if env("CLOUDTHINKER_NOTIFY").as_deref() == Some("0") {
        "notifications off".to_string()
    } else {
        let protocol = if kitty {
            "OSC 99"
        } else if term.starts_with("foot") || term.contains("rxvt") || program == "foot" {
            "OSC 777"
        } else {
            "OSC 9"
        };
        let tmux = if env("TMUX").is_some() {
            " through tmux"
        } else {
            ""
        };
        format!("notifications via {protocol}{tmux}")
    };
    let kitty_keyboard = kitty
        || matches!(program.as_str(), "ghostty" | "WezTerm" | "foot")
        || term.contains("ghostty")
        || term.starts_with("foot")
        || term.starts_with("alacritty")
        || env("GHOSTTY_RESOURCES_DIR").is_some()
        || env("WEZTERM_PANE").is_some()
        || (env("WT_SESSION").is_some() && !remote);
    let no_keys = program == "Apple_Terminal" || term == "dumb" || term == "linux";
    let name = [program.as_str(), term.as_str()]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or("Unknown terminal");
    let shift_enter = if kitty_keyboard {
        "Shift+Enter adds a line"
    } else if no_keys {
        "Shift+Enter sends Enter"
    } else {
        "Shift+Enter needs modifyOtherKeys"
    };
    let summary = format!("{name}: {colour}, {clipboard}, {notifications}, {shift_enter}");
    if no_keys {
        local(
            Area::Terminal,
            "terminal",
            "Terminal",
            Status::Warn,
            summary,
        )
        .fix(Fix::Step(
            "Use Ctrl+J for a new line, or a terminal such as Ghostty, kitty, or WezTerm.".into(),
        ))
    } else {
        local(
            Area::Terminal,
            "terminal",
            "Terminal",
            Status::Pass,
            summary,
        )
    }
}

fn project_check(cwd: Option<&Path>) -> ServerCheck {
    let found: Vec<&str> = ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .filter(|name| cwd.is_some_and(|cwd| cwd.join(name).is_file()))
        .collect();
    let summary = if found.is_empty() {
        "No AGENTS.md or CLAUDE.md in this folder".to_string()
    } else {
        format!("{} found", found.join(" and "))
    };
    local(Area::Project, "project", "Project", Status::Pass, summary)
}

fn agent_dir(home: Option<&Path>) -> Option<PathBuf> {
    env(AGENT_DIR_ENV_VAR)
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".cloudthinker").join("agent")))
}

fn skill_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| entry.path().join("SKILL.md").is_file())
            .count()
    })
}

fn project_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join(".cloudthinker").join("skills")];
    for dir in cwd.ancestors() {
        dirs.push(dir.join(".agents").join("skills"));
        if dir.join(".git").exists() {
            break;
        }
    }
    dirs
}

fn skills_check(home: Option<&Path>, cwd: Option<&Path>) -> ServerCheck {
    let agent = agent_dir(home);
    let personal: usize = agent
        .iter()
        .map(|dir| dir.join("skills"))
        .chain(home.map(|home| home.join(".agents").join("skills")))
        .map(|dir| skill_count(&dir))
        .sum();
    let project: usize = cwd
        .map(project_skill_dirs)
        .unwrap_or_default()
        .iter()
        .filter(|dir| home.is_none_or(|home| **dir != home.join(".agents").join("skills")))
        .map(|dir| skill_count(dir))
        .sum();
    let workspace: usize = agent
        .map(|dir| dir.join("cloudthinker").join("skills"))
        .and_then(|root| std::fs::read_dir(root).ok())
        .map_or(0, |entries| {
            entries
                .flatten()
                .map(|entry| skill_count(&entry.path()))
                .sum()
        });
    let built_in = agent_bin_root()
        .ok()
        .and_then(|root| installed_agent_binary(&root, CLI_VERSION))
        .and_then(|binary| {
            binary
                .parent()
                .map(|dir| skill_count(&dir.join(".agents").join("skills")))
        })
        .unwrap_or(0);
    local(
        Area::Agent,
        "skills",
        "Skills",
        Status::Pass,
        format!(
            "{} skills: {built_in} built in, {workspace} workspace, {personal} personal, {project} project",
            built_in + workspace + personal + project
        ),
    )
}

fn install_check() -> ServerCheck {
    let shown = |path: Option<PathBuf>| {
        path.map_or_else(|| "unknown".to_string(), |path| path.display().to_string())
    };
    let binary = shown(std::env::current_exe().ok());
    let data = shown(dirs_home().map(|home| home.join(".cloudthinker")));
    let bundles = shown(agent_bin_root().ok());
    let config = config_dir().ok();
    let login = if env(TOKEN_ENV_VAR).is_some() {
        format!("login from {TOKEN_ENV_VAR}")
    } else {
        format!(
            "login file {}",
            shown(config.as_ref().map(|dir| dir.join("credentials.json")))
        )
    };
    let config = shown(config);
    local(
        Area::Install,
        "install",
        "Install",
        Status::Pass,
        format!("binary {binary}; agent bundles {bundles}; settings {data}; {login}"),
    )
    .fix(Fix::Step(format!(
        "To remove CloudThinker, run `cloudthinker logout`, then delete {binary}, {data}, and {config}."
    )))
}
