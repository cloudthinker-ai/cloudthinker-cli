use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::host::try_lock;
use super::paths::{remote_root, validate_name};
use super::wire::{Catalog, Query, RecentFolder, RunningAgent};

const AGENT_DIR_ENV: &str = "CLOUDTHINKER_CODING_AGENT_DIR";
const SESSION_DIR_ENV: &str = "CLOUDTHINKER_CODING_AGENT_SESSION_DIR";
const META_SUFFIX: &str = ".meta";
const RECENT_LIMIT: usize = 8;
const COMPLETION_LIMIT: usize = 50;
const TITLE_CHARS: usize = 60;
const READ_BUDGET: u64 = 256 * 1024;
const HEAD_BYTES: u64 = 192 * 1024;
const TAIL_BYTES: u64 = READ_BUDGET - HEAD_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub cwd: String,
    pub started: u64,
}

impl Meta {
    pub fn render(&self) -> String {
        format!("cwd={}\nstarted={}\n", self.cwd, self.started)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut cwd = None;
        let mut started = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("cwd=") {
                cwd = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("started=") {
                started = value.trim().parse().ok();
            }
        }
        Some(Self {
            cwd: cwd.filter(|cwd| Path::new(cwd).is_absolute())?,
            started: started?,
        })
    }
}

pub fn build(query: &Query) -> Catalog {
    let home = home_folder();
    let sessions = env_folder(SESSION_DIR_ENV).unwrap_or_else(|| {
        env_folder(AGENT_DIR_ENV)
            .unwrap_or_else(|| home.join(".cloudthinker").join("agent"))
            .join("sessions")
    });
    let remote = remote_root().ok();
    build_from(query, &home, remote.as_deref(), &sessions)
}

fn build_from(
    query: &Query,
    home: &Path,
    remote_root: Option<&Path>,
    sessions_root: &Path,
) -> Catalog {
    let mut running = remote_root.map(running_agents).unwrap_or_default();
    let running_folders: Vec<PathBuf> = running.iter().map(|agent| canonical(&agent.cwd)).collect();
    let mut titled = vec![false; running.len()];
    let mut seen = HashSet::new();
    let mut recent = Vec::new();
    for session in history(sessions_root) {
        if recent.len() >= RECENT_LIMIT && titled.iter().all(|done| *done) {
            break;
        }
        if !seen.insert(session.cwd.clone()) {
            continue;
        }
        let folder = canonical(&session.cwd);
        if let Some(index) = running_folders
            .iter()
            .position(|running| *running == folder)
        {
            if let (Some(agent), Some(done)) = (running.get_mut(index), titled.get_mut(index)) {
                agent.title = session.title;
                *done = true;
            }
        } else if recent.len() < RECENT_LIMIT && Path::new(&session.cwd).is_dir() {
            recent.push(RecentFolder {
                origin: read_origin(Path::new(&session.cwd)),
                cwd: session.cwd,
                title: session.title,
                modified: session.modified,
                session: session.path.display().to_string(),
            });
        }
    }
    for agent in &mut running {
        agent.origin = read_origin(Path::new(&agent.cwd));
    }
    Catalog {
        home: home.display().to_string(),
        probe: query
            .probe
            .as_deref()
            .map(|typed| expand(typed, home))
            .filter(|path| path.is_dir() && path != home)
            .map(|path| path.display().to_string()),
        running,
        recent,
    }
}

fn canonical(folder: &str) -> PathBuf {
    std::fs::canonicalize(folder).unwrap_or_else(|_| PathBuf::from(folder))
}

pub fn home_folder() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

pub fn expand(typed: &str, home: &Path) -> PathBuf {
    match typed.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => home.join(typed),
    }
}

pub fn tilde(path: &Path, home: &Path) -> String {
    if home.parent().is_none() {
        return path.display().to_string();
    }
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn complete(typed: &str, home: &Path) -> Vec<String> {
    let (folder, partial) = match typed.rfind('/') {
        Some(at) => typed.split_at(at + 1),
        None if typed == "~" => return vec!["~/".into()],
        None => ("", typed),
    };
    let Ok(entries) = std::fs::read_dir(expand(folder, home)) else {
        return Vec::new();
    };
    let hidden = partial.starts_with('.');
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let wanted = name.starts_with(partial) && (hidden || !name.starts_with('.'));
            (wanted && entry.path().is_dir()).then_some(name)
        })
        .collect();
    names.sort_unstable();
    names
        .into_iter()
        .take(COMPLETION_LIMIT)
        .map(|name| format!("{folder}{name}/"))
        .collect()
}

pub fn read_origin(dir: &Path) -> Option<String> {
    let config = dir.ancestors().find_map(git_config)?;
    let text = std::fs::read_to_string(config).ok()?;
    origin_url(&text).map(normalize_origin)
}

pub fn normalize_origin(url: &str) -> String {
    let url = url.trim();
    let (authority, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/').unwrap_or((rest, "")),
        None => match url.split_once(':') {
            Some((authority, path)) if !authority.contains('/') => (authority, path),
            _ => return trim_repo(url).to_string(),
        },
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    format!(
        "{}/{}",
        host.to_ascii_lowercase(),
        trim_repo(path.trim_start_matches('/'))
    )
}

fn trim_repo(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/')
}

fn git_config(folder: &Path) -> Option<PathBuf> {
    let dot_git = folder.join(".git");
    let metadata = std::fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.join("config"));
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = folder.join(pointer.lines().next()?.strip_prefix("gitdir:")?.trim());
    Some(match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => gitdir.join(common.trim()).join("config"),
        Err(_) => gitdir.join("config"),
    })
}

fn origin_url(config: &str) -> Option<&str> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = is_origin_section(line);
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("url")
        {
            return Some(value.trim().trim_matches('"'));
        }
    }
    None
}

fn is_origin_section(line: &str) -> bool {
    let Some(inner) = line
        .strip_prefix('[')
        .and_then(|rest| rest.split(']').next())
    else {
        return false;
    };
    inner
        .split_once(char::is_whitespace)
        .is_some_and(|(section, name)| {
            section.eq_ignore_ascii_case("remote") && name.trim() == "\"origin\""
        })
}

fn env_folder(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

fn running_agents(root: &Path) -> Vec<RunningAgent> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut running: Vec<RunningAgent> = entries
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let name = file.to_str()?.strip_suffix(META_SUFFIX)?;
            validate_name(name).ok()?;
            if !is_held(&root.join(format!("{name}.lock"))) {
                return None;
            }
            let meta = Meta::parse(&std::fs::read_to_string(entry.path()).ok()?)?;
            Some(RunningAgent {
                name: name.to_string(),
                cwd: meta.cwd,
                started_at: meta.started,
                title: String::new(),
                origin: None,
            })
        })
        .collect();
    running.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    running
}

fn is_held(lock: &Path) -> bool {
    lock.is_file() && matches!(try_lock(lock), Ok(None))
}

struct Session {
    cwd: String,
    title: String,
    modified: u64,
    path: PathBuf,
}

fn history(root: &Path) -> impl Iterator<Item = Session> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|folder| folder.path())
        .collect();
    paths.sort();
    let mut folders: Vec<Vec<(SystemTime, PathBuf)>> = paths
        .iter()
        .filter_map(|folder| session_files(folder))
        .collect();
    let mut queue: BinaryHeap<(SystemTime, Reverse<usize>)> = folders
        .iter()
        .enumerate()
        .filter_map(|(index, files)| Some((files.last()?.0, Reverse(index))))
        .collect();
    std::iter::from_fn(move || {
        while let Some((_, Reverse(index))) = queue.pop() {
            let files = folders.get_mut(index)?;
            let (modified, path) = files.pop()?;
            if let Some(session) = read_session(path, unix_seconds(modified)) {
                return Some(session);
            }
            if let Some(next) = files.last() {
                queue.push((next.0, Reverse(index)));
            }
        }
        None
    })
}

fn session_files(folder: &Path) -> Option<Vec<(SystemTime, PathBuf)>> {
    let mut files: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            let metadata = entry.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((metadata.modified().ok()?, path))
        })
        .collect();
    files.sort_by_key(|file| file.0);
    (!files.is_empty()).then_some(files)
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "type")]
    kind: String,
    cwd: Option<String>,
    #[serde(rename = "parentSession")]
    parent_session: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct Tagged {
    #[serde(rename = "type")]
    kind: Kind,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    SessionInfo,
    Message,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct SessionInfo {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    role: String,
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl Content {
    fn into_text(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Parts(parts) => parts
                .into_iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text)
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

fn read_session(path: PathBuf, modified: u64) -> Option<Session> {
    let file = File::open(&path).ok()?;
    let length = file.metadata().ok()?.len();
    let head_limit = if length <= READ_BUDGET {
        READ_BUDGET
    } else {
        HEAD_BYTES
    };
    let mut head_reader = BufReader::new(file).take(head_limit);
    let mut first = Vec::new();
    head_reader.read_until(b'\n', &mut first).ok()?;
    let header: Header = serde_json::from_slice(&first).ok()?;
    if header.kind != "session" || header.parent_session.is_some() {
        return None;
    }
    let cwd = header.cwd.filter(|cwd| !cwd.is_empty())?;
    let mut head = Vec::new();
    head_reader.read_to_end(&mut head).ok()?;
    let mut tail = Vec::new();
    if length > READ_BUDGET {
        let mut file = head_reader.into_inner().into_inner();
        file.seek(SeekFrom::Start(length - TAIL_BYTES)).ok()?;
        file.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
        let start = tail
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(tail.len(), |at| at + 1);
        tail.drain(..start);
    }
    let title = session_title(
        head.split(|byte| *byte == b'\n')
            .chain(tail.split(|byte| *byte == b'\n')),
    );
    Some(Session {
        cwd,
        title,
        modified,
        path: std::path::absolute(&path).unwrap_or(path),
    })
}

fn session_title<'a>(lines: impl Iterator<Item = &'a [u8]>) -> String {
    let mut named = None;
    let mut first_message = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let Ok(tagged) = serde_json::from_slice::<Tagged>(line) else {
            continue;
        };
        match tagged.kind {
            Kind::SessionInfo => {
                if let Ok(SessionInfo { name: Some(name) }) = serde_json::from_slice(line)
                    && !name.trim().is_empty()
                {
                    named = Some(name);
                }
            }
            Kind::Message if first_message.is_none() => {
                if let Ok(Envelope { message }) = serde_json::from_slice(line)
                    && message.role == "user"
                    && let Some(text) = message.content.map(Content::into_text)
                    && !text.trim().is_empty()
                {
                    first_message = Some(text);
                }
            }
            Kind::Message | Kind::Other => {}
        }
    }
    named
        .or(first_message)
        .map(|text| short_title(&text))
        .unwrap_or_default()
}

fn short_title(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_CHARS {
        return collapsed;
    }
    let mut short: String = collapsed.chars().take(TITLE_CHARS - 1).collect();
    short.truncate(short.trim_end().len());
    short.push('…');
    short
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(folder: &Path, file: &str, lines: &[String], age_secs: u64) -> PathBuf {
        std::fs::create_dir_all(folder).unwrap();
        let path = folder.join(file);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let modified = SystemTime::now() - std::time::Duration::from_secs(age_secs);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }

    fn header(cwd: &Path) -> String {
        format!(
            r#"{{"type":"session","version":3,"id":"a","timestamp":"2026-10-18T16:07:53.646Z","cwd":"{}"}}"#,
            cwd.display()
        )
    }

    fn user(text: &str) -> String {
        format!(
            r#"{{"type":"message","id":"b","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}],"timestamp":1}}}}"#
        )
    }

    #[test]
    fn a_folder_ranks_by_the_session_it_shows_not_a_newer_child_session() {
        let temporary = tempfile::tempdir().unwrap();
        let sessions = temporary.path().join("sessions");
        let (first, second) = (temporary.path().join("a"), temporary.path().join("b"));
        write_session(
            &sessions.join("--a--"),
            "top.jsonl",
            &[header(&first), user("a")],
            50,
        );
        write_session(
            &sessions.join("--a--"),
            "child.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"s","cwd":"/a","parentSession":"/x.jsonl"}"#
                    .into(),
                user("child"),
            ],
            10,
        );
        write_session(
            &sessions.join("--b--"),
            "top.jsonl",
            &[header(&second), user("b")],
            30,
        );
        let order: Vec<String> = history(&sessions).map(|session| session.cwd).collect();
        assert_eq!(
            order,
            [second.display().to_string(), first.display().to_string()]
        );
    }

    #[test]
    fn the_catalog_lists_running_agents_and_recent_folders() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let remote = temporary.path().join("remote");
        let sessions = temporary.path().join("agent").join("sessions");
        std::fs::create_dir_all(&remote).unwrap();
        let mut folders = Vec::new();
        for index in 0..10 {
            let folder = home.join(format!("app{index}"));
            std::fs::create_dir_all(&folder).unwrap();
            write_session(
                &sessions.join(format!("--app{index}--")),
                "s.jsonl",
                &[header(&folder), user(&format!("task   {index}\\n  now"))],
                100 + index * 10,
            );
            folders.push(folder);
        }
        let named = home.join("named");
        std::fs::create_dir_all(&named).unwrap();
        write_session(
            &sessions.join("--named--"),
            "old.jsonl",
            &[
                header(&named),
                user("first ask"),
                r#"{"type":"session_info","id":"c","name":"old name"}"#.into(),
                r#"{"type":"session_info","id":"d","name":"fix login bug"}"#.into(),
            ],
            50,
        );
        write_session(
            &sessions.join("--named--"),
            "sub.jsonl",
            &[
                r#"{"type":"session","version":3,"id":"s","cwd":"/elsewhere","parentSession":"/x.jsonl"}"#.into(),
                user("subagent work"),
            ],
            1,
        );
        write_session(
            &sessions.join("--gone--"),
            "s.jsonl",
            &[header(&home.join("gone")), user("lost")],
            2,
        );
        let busy = home.join("busy");
        std::fs::create_dir_all(&busy).unwrap();
        write_session(
            &sessions.join("--busy--"),
            "s.jsonl",
            &[header(&busy), user(&"long ".repeat(30))],
            3,
        );
        let busy_link = home.join("busy-link");
        std::os::unix::fs::symlink(&busy, &busy_link).unwrap();
        std::fs::write(
            remote.join("busy-1.meta"),
            Meta {
                cwd: busy_link.display().to_string(),
                started: 42,
            }
            .render(),
        )
        .unwrap();
        let held = try_lock(&remote.join("busy-1.lock")).unwrap();
        assert!(held.is_some());
        std::fs::write(
            remote.join("stale-1.meta"),
            Meta {
                cwd: named.display().to_string(),
                started: 7,
            }
            .render(),
        )
        .unwrap();
        std::fs::write(remote.join("stale-1.lock"), "").unwrap();

        let catalog = build_from(
            &Query {
                probe: Some("~/app3".into()),
            },
            &home,
            Some(remote.as_path()),
            &sessions,
        );

        assert_eq!(catalog.home, home.display().to_string());
        assert_eq!(catalog.probe, Some(folders[3].display().to_string()));
        assert_eq!(catalog.running.len(), 1);
        let running = &catalog.running[0];
        assert_eq!(running.name, "busy-1");
        assert_eq!(running.cwd, busy_link.display().to_string());
        assert_eq!(running.started_at, 42);
        assert_eq!(running.title.chars().count(), TITLE_CHARS);
        assert!(running.title.ends_with("long…"));
        let recent: Vec<&str> = catalog
            .recent
            .iter()
            .map(|folder| folder.title.as_str())
            .collect();
        assert_eq!(
            recent,
            [
                "fix login bug",
                "task 0 now",
                "task 1 now",
                "task 2 now",
                "task 3 now",
                "task 4 now",
                "task 5 now",
                "task 6 now"
            ]
        );
        assert_eq!(catalog.recent[0].cwd, named.display().to_string());
        assert!(catalog.recent[0].session.ends_with("old.jsonl"));
        assert!(
            catalog
                .recent
                .windows(2)
                .all(|pair| pair[0].modified >= pair[1].modified)
        );
        drop(held);
    }

    #[test]
    fn the_probe_skips_home_and_missing_folders() {
        let home = tempfile::tempdir().unwrap();
        let empty = home.path().join("none");
        for probe in ["~", "~/", "~/missing"] {
            let catalog = build_from(
                &Query {
                    probe: Some(probe.into()),
                },
                home.path(),
                Some(empty.as_path()),
                &empty,
            );
            assert_eq!(catalog.probe, None, "{probe}");
            assert!(catalog.running.is_empty() && catalog.recent.is_empty());
        }
    }

    #[test]
    fn a_title_comes_from_a_string_message_when_unnamed() {
        let lines: [&[u8]; 4] = [
            br#"{"type":"message","message":{"role":"system","content":"rules"}}"#,
            br#"{"type":"message","message":{"role":"user","content":"  hello\tthere  "}}"#,
            br#"{"type":"message","message":{"role":"user","content":"second"}}"#,
            b"{broken",
        ];
        assert_eq!(session_title(lines.into_iter()), "hello there");
        assert_eq!(session_title(std::iter::empty::<&[u8]>()), "");
    }

    #[test]
    fn origins_compare_without_scheme_user_port_or_suffix() {
        for (url, expected) in [
            ("git@gitlab.com:Team/App.git", "gitlab.com/Team/App"),
            ("https://gitlab.com/Team/App", "gitlab.com/Team/App"),
            (
                "https://user:token@GitLab.com/Team/App.git/",
                "gitlab.com/Team/App",
            ),
            (
                "ssh://git@gitlab.com:2222/Team/App.git",
                "gitlab.com/Team/App",
            ),
            ("git@github.com:/org/repo", "github.com/org/repo"),
            ("gitlab.com:Team/App", "gitlab.com/Team/App"),
            ("/srv/git/app.git", "/srv/git/app"),
        ] {
            assert_eq!(normalize_origin(url), expected, "{url}");
        }
    }

    #[test]
    fn the_origin_is_read_from_a_repo_or_a_worktree() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[core]\n\turl = wrong\n[remote \"upstream\"]\n\turl = git@x.com:a/b.git\n[remote \"origin\"]\n\tfetch = +refs/heads/*\n\tURL = \"git@GitLab.com:Team/App.git\"\n",
        )
        .unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        let worktree = temporary.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert_eq!(
            read_origin(&repo.join("src/deep")).as_deref(),
            Some("gitlab.com/Team/App")
        );
        assert_eq!(
            read_origin(&worktree).as_deref(),
            Some("gitlab.com/Team/App")
        );
    }

    #[test]
    fn completions_list_folders_in_the_typed_form() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        for folder in ["app", "apps", "api", ".config", "other"] {
            std::fs::create_dir(home.join(folder)).unwrap();
        }
        std::fs::write(home.join("apple.txt"), "").unwrap();
        std::fs::create_dir_all(home.join("app/src")).unwrap();
        assert_eq!(complete("~/ap", home), ["~/api/", "~/app/", "~/apps/"]);
        assert_eq!(complete("ap", home), ["api/", "app/", "apps/"]);
        assert_eq!(complete("~/app/", home), ["~/app/src/"]);
        assert_eq!(complete("~/.c", home), ["~/.config/"]);
        assert_eq!(
            complete("~/", home),
            ["~/api/", "~/app/", "~/apps/", "~/other/"]
        );
        assert_eq!(complete("~", home), ["~/"]);
        let absolute = format!("{}/ot", home.display());
        assert_eq!(
            complete(&absolute, home),
            [format!("{}/other/", home.display())]
        );
        assert!(complete("~/missing/", home).is_empty());
        for index in 0..60 {
            std::fs::create_dir(home.join(format!("many{index:02}"))).unwrap();
        }
        let many = complete("~/many", home);
        assert_eq!(many.len(), COMPLETION_LIMIT);
        assert_eq!(many[0], "~/many00/");
    }

    #[test]
    fn meta_round_trips_and_rejects_partial_files() {
        let meta = Meta {
            cwd: "/srv/my app".into(),
            started: 1_760_000_000,
        };
        assert_eq!(Meta::parse(&meta.render()), Some(meta));
        assert_eq!(Meta::parse("cwd=/srv/app\n"), None);
        assert_eq!(Meta::parse("started=5\n"), None);
        assert_eq!(Meta::parse("cwd=relative\nstarted=5\n"), None);
    }

    #[test]
    fn folders_show_home_as_a_tilde() {
        let home = Path::new("/home/me");
        assert_eq!(tilde(home, home), "~");
        assert_eq!(tilde(&home.join("app"), home), "~/app");
        assert_eq!(tilde(Path::new("/srv/app"), home), "/srv/app");
        assert_eq!(tilde(Path::new("/srv/app"), Path::new("/")), "/srv/app");
    }
}
