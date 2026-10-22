use std::path::Path;

use chrono::{DateTime, Local};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::catalog::{expand, tilde};
use super::paths::name_for_folder;
use super::wire::{Catalog, RunningAgent, Size};

const MAX_MATCHES: usize = 8;
const MAX_PATH: usize = 4096;
const MIN_DETAIL_COLUMN: usize = 22;
const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const YEAR_SHOWN_AFTER: u64 = 300 * DAY;
const ESC: u8 = 0x1b;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub file: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Attach {
        name: String,
    },
    Start {
        name: String,
        cwd: String,
        session: Option<Session>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Open(Decision),
    Ask,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Explicit<'a> {
    pub name: Option<&'a str>,
    pub cwd: Option<&'a str>,
}

pub fn choose(catalog: &Catalog, laptop_origin: Option<&str>, explicit: Explicit<'_>) -> Choice {
    Choice::Open(if let Some(name) = explicit.name {
        if catalog.running.iter().any(|agent| agent.name == name) {
            Decision::Attach {
                name: name.to_string(),
            }
        } else {
            let cwd = resolve(explicit.cwd.unwrap_or("~"), &catalog.home);
            Decision::Start {
                name: name.to_string(),
                session: session_for(catalog, &cwd),
                cwd,
            }
        }
    } else if let Some(cwd) = explicit.cwd {
        open_folder(catalog, &resolve(cwd, &catalog.home), true)
    } else if let Some(agent) = laptop_origin.and_then(|origin| {
        catalog
            .running
            .iter()
            .find(|agent| agent.origin.as_deref() == Some(origin))
    }) {
        attach(agent)
    } else if let Some(folder) = laptop_origin.and_then(|origin| {
        catalog
            .recent
            .iter()
            .find(|folder| folder.origin.as_deref() == Some(origin))
    }) {
        open_folder(catalog, &folder.cwd, true)
    } else if let Some(probe) = &catalog.probe {
        open_folder(catalog, probe, true)
    } else {
        match catalog.running.as_slice() {
            [agent] => attach(agent),
            [] if catalog.recent.is_empty() => open_folder(catalog, &catalog.home, true),
            _ => return Choice::Ask,
        }
    })
}

pub fn resolve(typed: &str, home: &str) -> String {
    trim_folder(&expand(typed, Path::new(home)).to_string_lossy()).to_string()
}

pub fn display_folder(cwd: &str, home: &str) -> String {
    tilde(Path::new(cwd), Path::new(home))
}

pub fn uptime(now: u64, started: u64) -> String {
    let minutes = now.saturating_sub(started) / MINUTE;
    let (days, hours, minutes) = (minutes / (DAY / MINUTE), minutes / 60 % 24, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "up <1m".into(),
        (0, 0, _) => format!("up {minutes}m"),
        (0, _, _) => format!("up {hours}h {minutes}m"),
        _ => format!("up {days}d {hours}h"),
    }
}

pub fn ago(now: u64, then: u64) -> String {
    let age = now.saturating_sub(then);
    if age < MINUTE {
        "just now".into()
    } else if age < HOUR {
        format!("{}m ago", age / MINUTE)
    } else if age < DAY {
        format!("{}h ago", age / HOUR)
    } else if age < 2 * DAY {
        "yesterday".into()
    } else if age < 7 * DAY {
        format!("{} days ago", age / DAY)
    } else if age < 14 * DAY {
        "last week".into()
    } else {
        date(then, age >= YEAR_SHOWN_AFTER)
    }
}

fn date(then: u64, with_year: bool) -> String {
    let Some(utc) = i64::try_from(then)
        .ok()
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
    else {
        return String::new();
    };
    let local = utc.with_timezone(&Local);
    let format = if with_year { "%b %-d %Y" } else { "%b %-d" };
    local.format(format).to_string()
}

fn attach(agent: &RunningAgent) -> Decision {
    Decision::Attach {
        name: agent.name.clone(),
    }
}

fn open_folder(catalog: &Catalog, cwd: &str, keep_session: bool) -> Decision {
    if let Some(agent) = catalog
        .running
        .iter()
        .find(|agent| same_folder(&agent.cwd, cwd))
    {
        return attach(agent);
    }
    let cwd = trim_folder(cwd);
    Decision::Start {
        name: name_for_folder(cwd),
        session: if keep_session {
            session_for(catalog, cwd)
        } else {
            None
        },
        cwd: cwd.to_string(),
    }
}

fn session_for(catalog: &Catalog, cwd: &str) -> Option<Session> {
    catalog
        .recent
        .iter()
        .find(|folder| same_folder(&folder.cwd, cwd))
        .map(|folder| Session {
            file: folder.session.clone(),
            title: folder.title.clone(),
        })
}

fn trim_folder(path: &str) -> &str {
    match path.trim_end_matches('/') {
        "" if path.starts_with('/') => "/",
        trimmed => trimmed,
    }
}

fn same_folder(left: &str, right: &str) -> bool {
    trim_folder(left) == trim_folder(right)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Escape,
    Backspace,
    Tab,
    Interrupt,
    Char(char),
}

pub fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = bytes;
    while let Some((&first, tail)) = rest.split_first() {
        let used = match first {
            ESC => match tail.first() {
                Some(b'[' | b'O') => {
                    let Some(at) = tail
                        .iter()
                        .skip(1)
                        .position(|byte| (0x40..=0x7e).contains(byte))
                    else {
                        break;
                    };
                    match tail.get(at + 1) {
                        Some(b'A') => keys.push(Key::Up),
                        Some(b'B') => keys.push(Key::Down),
                        _ => {}
                    }
                    at + 3
                }
                _ => {
                    keys.push(Key::Escape);
                    1
                }
            },
            b'\r' | b'\n' => {
                keys.push(Key::Enter);
                1
            }
            0x7f | 0x08 => {
                keys.push(Key::Backspace);
                1
            }
            b'\t' => {
                keys.push(Key::Tab);
                1
            }
            0x03 => {
                keys.push(Key::Interrupt);
                1
            }
            0x00..=0x1f => 1,
            _ => {
                let len = rest
                    .iter()
                    .position(|&byte| byte < 0x20 || byte == 0x7f)
                    .unwrap_or(rest.len());
                let text = rest.get(..len).unwrap_or_default();
                keys.extend(String::from_utf8_lossy(text).chars().map(Key::Char));
                len
            }
        };
        rest = rest.get(used..).unwrap_or_default();
    }
    keys
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Nothing,
    Redraw,
    Complete(String),
    Done(Decision),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Running(usize),
    Recent(usize),
    Home,
    Other,
}

#[derive(Debug)]
enum Mode {
    List,
    Path { text: String, matches: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Strong,
    Dim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub lines: Vec<Line>,
    pub cursor: Option<(usize, usize)>,
}

pub struct Picker<'a> {
    catalog: &'a Catalog,
    target: &'a str,
    now: u64,
    filter: String,
    selected: usize,
    mode: Mode,
}

impl<'a> Picker<'a> {
    pub fn new(catalog: &'a Catalog, target: &'a str, now: u64) -> Self {
        Self {
            catalog,
            target,
            now,
            filter: String::new(),
            selected: 0,
            mode: Mode::List,
        }
    }

    pub fn press(&mut self, keys: &[Key]) -> Action {
        let mut action = Action::Nothing;
        for &key in keys {
            match self.key(key) {
                Action::Nothing => {}
                Action::Redraw => action = Action::Redraw,
                other => return other,
            }
        }
        action
    }

    pub fn completions(&mut self, entries: Vec<String>) -> Action {
        let Mode::Path { text, matches } = &mut self.mode else {
            return Action::Nothing;
        };
        if let Some(common) = common_prefix(&entries)
            && common.len() > text.len()
            && common.starts_with(text.as_str())
        {
            *text = common.to_string();
        }
        *matches = if entries.len() > 1 {
            entries.into_iter().take(MAX_MATCHES).collect()
        } else {
            Vec::new()
        };
        Action::Redraw
    }

    pub fn view(&self, size: Size) -> View {
        let width = usize::from(size.cols);
        let height = usize::from(size.rows).saturating_sub(1).max(3);
        match &self.mode {
            Mode::List => self.list_view(width, height),
            Mode::Path { text, matches } => self.path_view(text, matches, width, height),
        }
    }

    fn key(&mut self, key: Key) -> Action {
        if key == Key::Interrupt {
            Action::Cancel
        } else if matches!(self.mode, Mode::List) {
            self.list_key(key)
        } else {
            self.path_key(key)
        }
    }

    fn list_key(&mut self, key: Key) -> Action {
        let rows = self.rows();
        let last = rows.len().saturating_sub(1);
        let current = rows
            .get(self.selected.min(last))
            .copied()
            .unwrap_or(Row::Other);
        match key {
            Key::Up => {
                self.selected = self.selected.min(last).saturating_sub(1);
                Action::Redraw
            }
            Key::Down => {
                self.selected = (self.selected + 1).min(last);
                Action::Redraw
            }
            Key::Enter => self.open(current, true),
            Key::Char('n')
                if self.filter.is_empty() && matches!(current, Row::Recent(_) | Row::Home) =>
            {
                self.open(current, false)
            }
            Key::Char(ch) => {
                if self.filter.len() + ch.len_utf8() <= MAX_PATH {
                    self.filter.push(ch);
                }
                self.selected = 0;
                Action::Redraw
            }
            Key::Backspace if self.filter.pop().is_some() => {
                self.selected = 0;
                Action::Redraw
            }
            Key::Escape => Action::Cancel,
            Key::Backspace | Key::Tab | Key::Interrupt => Action::Nothing,
        }
    }

    fn path_key(&mut self, key: Key) -> Action {
        let catalog = self.catalog;
        let Mode::Path { text, matches } = &mut self.mode else {
            return Action::Nothing;
        };
        match key {
            Key::Char(ch) => {
                if text.len() + ch.len_utf8() <= MAX_PATH {
                    text.push(ch);
                }
                matches.clear();
                Action::Redraw
            }
            Key::Backspace => {
                text.pop();
                matches.clear();
                Action::Redraw
            }
            Key::Tab => {
                if text.is_empty() {
                    text.push_str("~/");
                }
                Action::Complete(text.clone())
            }
            Key::Enter if text.trim().is_empty() => Action::Nothing,
            Key::Enter => {
                let cwd = resolve(text.trim(), &catalog.home);
                Action::Done(open_folder(catalog, &cwd, true))
            }
            Key::Escape => {
                self.mode = Mode::List;
                Action::Redraw
            }
            Key::Up | Key::Down | Key::Interrupt => Action::Nothing,
        }
    }

    fn open(&mut self, row: Row, keep_session: bool) -> Action {
        let catalog = self.catalog;
        match row {
            Row::Running(index) => catalog
                .running
                .get(index)
                .map_or(Action::Nothing, |agent| Action::Done(attach(agent))),
            Row::Recent(index) => catalog.recent.get(index).map_or(Action::Nothing, |folder| {
                Action::Done(open_folder(catalog, &folder.cwd, keep_session))
            }),
            Row::Home => Action::Done(open_folder(catalog, &catalog.home, keep_session)),
            Row::Other => {
                let text = if self.filter.starts_with(['~', '/']) {
                    std::mem::take(&mut self.filter)
                } else {
                    String::new()
                };
                self.mode = Mode::Path {
                    text,
                    matches: Vec::new(),
                };
                Action::Redraw
            }
        }
    }

    fn rows(&self) -> Vec<Row> {
        let needle = self.filter.to_lowercase();
        let home = &self.catalog.home;
        let mut rows: Vec<Row> = Vec::new();
        for (index, agent) in self.catalog.running.iter().enumerate() {
            if self.shows(&needle, &agent.cwd, &agent.title) {
                rows.push(Row::Running(index));
            }
        }
        for (index, folder) in self.catalog.recent.iter().enumerate() {
            if !same_folder(&folder.cwd, home) && self.shows(&needle, &folder.cwd, &folder.title) {
                rows.push(Row::Recent(index));
            }
        }
        let home_title = session_for(self.catalog, home).map(|session| session.title);
        if "home".contains(&needle)
            || self.shows(&needle, home, home_title.as_deref().unwrap_or(""))
        {
            rows.push(Row::Home);
        }
        rows.push(Row::Other);
        rows
    }

    fn shows(&self, needle: &str, cwd: &str, title: &str) -> bool {
        needle.is_empty()
            || display_folder(cwd, &self.catalog.home)
                .to_lowercase()
                .contains(needle)
            || title.to_lowercase().contains(needle)
    }

    fn list_view(&self, width: usize, height: usize) -> View {
        let rows = self.rows();
        let selected = self.selected.min(rows.len().saturating_sub(1));
        let hint = if self.filter.is_empty() {
            "type to filter".to_string()
        } else {
            format!("filter: {}", self.filter)
        };
        let column = rows
            .iter()
            .map(|row| self.row_lead(*row, false).width() + 2)
            .max()
            .unwrap_or(0)
            .max(MIN_DETAIL_COLUMN);
        let mut lines = vec![self.header(&hint, width)];
        let mut focus = 0;
        let mut in_recent = false;
        if rows.iter().any(|row| matches!(row, Row::Running(_))) {
            lines.push(dim(" Running"));
        }
        for (index, row) in rows.iter().enumerate() {
            if !in_recent && !matches!(row, Row::Running(_)) {
                in_recent = true;
                lines.push(dim(" Recent folders"));
            }
            let lead = self.row_lead(*row, index == selected);
            let pad = column.saturating_sub(lead.width());
            if index == selected {
                focus = lines.len();
            }
            lines.push(Line {
                text: format!("{lead}{:pad$}{}", "", self.row_detail(*row)),
                style: if index == selected {
                    Style::Strong
                } else {
                    Style::Plain
                },
            });
        }
        lines.push(dim(" ↑↓ choose · enter open · n new session · esc cancel"));
        View {
            lines: crop(lines, focus, height),
            cursor: None,
        }
    }

    fn path_view(&self, text: &str, matches: &[String], width: usize, height: usize) -> View {
        let prompt = format!(" Folder on {}: {text}", self.target);
        let column = prompt.width().min(width.saturating_sub(1));
        let mut lines = vec![
            self.header("type a path", width),
            Line {
                text: prompt,
                style: Style::Plain,
            },
        ];
        lines.extend(matches.iter().map(|entry| dim(&format!("   {entry}"))));
        lines.push(dim(" tab completes · enter start · esc back"));
        View {
            lines: crop(lines, 1, height),
            cursor: Some((1, column)),
        }
    }

    fn header(&self, hint: &str, width: usize) -> Line {
        let left = format!("cloudthinker remote · {}", self.target);
        let pad = width.saturating_sub(left.width() + hint.width() + 1).max(2);
        Line {
            text: format!("{left}{:pad$}{hint}", ""),
            style: Style::Plain,
        }
    }

    fn row_lead(&self, row: Row, selected: bool) -> String {
        let marker = if selected { "▸" } else { " " };
        let home = &self.catalog.home;
        let (dot, folder) = match row {
            Row::Running(index) => (
                "● ",
                self.catalog
                    .running
                    .get(index)
                    .map(|agent| display_folder(&agent.cwd, home))
                    .unwrap_or_default(),
            ),
            Row::Recent(index) => (
                "",
                self.catalog
                    .recent
                    .get(index)
                    .map(|folder| display_folder(&folder.cwd, home))
                    .unwrap_or_default(),
            ),
            Row::Home => ("", "~".to_string()),
            Row::Other => ("", "Other folder…".to_string()),
        };
        format!(" {marker} {dot}{folder}")
    }

    fn row_detail(&self, row: Row) -> String {
        match row {
            Row::Running(index) => self
                .catalog
                .running
                .get(index)
                .map(|agent| detail(&agent.title, &uptime(self.now, agent.started_at)))
                .unwrap_or_default(),
            Row::Recent(index) => self
                .catalog
                .recent
                .get(index)
                .map(|folder| detail(&folder.title, &ago(self.now, folder.modified)))
                .unwrap_or_default(),
            Row::Home => self
                .catalog
                .recent
                .iter()
                .find(|folder| same_folder(&folder.cwd, &self.catalog.home))
                .map_or_else(
                    || "home".to_string(),
                    |folder| detail(&folder.title, &ago(self.now, folder.modified)),
                ),
            Row::Other => "type a path · tab completes".into(),
        }
    }
}

fn detail(title: &str, time: &str) -> String {
    if title.is_empty() {
        time.to_string()
    } else {
        format!("\"{title}\" · {time}")
    }
}

fn dim(text: &str) -> Line {
    Line {
        text: text.to_string(),
        style: Style::Dim,
    }
}

fn crop(mut lines: Vec<Line>, focus: usize, height: usize) -> Vec<Line> {
    if lines.len() <= height {
        return lines;
    }
    let Some(footer) = lines.pop() else {
        return lines;
    };
    let room = height.saturating_sub(2).max(1);
    let start = focus
        .saturating_sub(room - 1)
        .max(1)
        .min(lines.len().saturating_sub(room));
    let mut kept: Vec<Line> = lines.drain(..1).collect();
    kept.extend(lines.into_iter().skip(start - 1).take(room));
    kept.push(footer);
    kept
}

fn common_prefix(entries: &[String]) -> Option<&str> {
    let (first, rest) = entries.split_first()?;
    let end = rest.iter().fold(first.len(), |end, entry| {
        first
            .get(..end)
            .unwrap_or_default()
            .char_indices()
            .zip(entry.chars())
            .find(|((_, left), right)| left != right)
            .map_or(end.min(entry.len()), |((at, _), _)| at)
    });
    first.get(..end)
}

fn fit(text: &str, width: usize) -> &str {
    let mut used = 0;
    for (at, char) in text.char_indices() {
        used += char.width().unwrap_or(0);
        if used > width {
            return text.get(..at).unwrap_or_default();
        }
    }
    text
}

#[derive(Debug, Default)]
pub struct Screen {
    cursor_line: usize,
    drawn: bool,
}

impl Screen {
    pub fn draw(&mut self, view: &View, cols: u16) -> Vec<u8> {
        let mut out = self.rewind();
        let width = usize::from(cols).saturating_sub(1).max(1);
        for (index, line) in view.lines.iter().enumerate() {
            if index > 0 {
                out.extend_from_slice(b"\r\n");
            }
            let text = fit(&line.text, width);
            let (on, off) = match line.style {
                Style::Plain => ("", ""),
                Style::Strong => ("\x1b[1m", "\x1b[0m"),
                Style::Dim => ("\x1b[2m", "\x1b[0m"),
            };
            out.extend_from_slice(format!("{on}{text}{off}").as_bytes());
        }
        let last = view.lines.len().saturating_sub(1);
        match view.cursor {
            Some((line, column)) => {
                let line = line.min(last);
                if last > line {
                    out.extend_from_slice(format!("\x1b[{}A", last - line).as_bytes());
                }
                out.push(b'\r');
                if column > 0 {
                    out.extend_from_slice(format!("\x1b[{column}C").as_bytes());
                }
                out.extend_from_slice(b"\x1b[?25h");
                self.cursor_line = line;
            }
            None => {
                out.extend_from_slice(b"\x1b[?25l");
                self.cursor_line = last;
            }
        }
        self.drawn = true;
        out
    }

    pub fn clear(&mut self) -> Vec<u8> {
        let mut out = self.rewind();
        out.extend_from_slice(b"\x1b[?25h");
        self.drawn = false;
        self.cursor_line = 0;
        out
    }

    fn rewind(&self) -> Vec<u8> {
        if !self.drawn {
            return Vec::new();
        }
        let mut out = b"\r".to_vec();
        if self.cursor_line > 0 {
            out.extend_from_slice(format!("\x1b[{}A", self.cursor_line).as_bytes());
        }
        out.extend_from_slice(b"\x1b[J");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::wire::RecentFolder;
    use super::*;

    const NOW: u64 = 1_760_000_000;
    const SIZE: Size = Size {
        rows: 40,
        cols: 100,
    };

    fn running(name: &str, cwd: &str, title: &str, origin: Option<&str>) -> RunningAgent {
        RunningAgent {
            name: name.into(),
            cwd: cwd.into(),
            started_at: NOW - 2 * HOUR - 14 * MINUTE,
            title: title.into(),
            origin: origin.map(String::from),
        }
    }

    fn recent(cwd: &str, title: &str, age: u64, origin: Option<&str>) -> RecentFolder {
        RecentFolder {
            cwd: cwd.into(),
            title: title.into(),
            modified: NOW - age,
            session: format!("{cwd}/session.jsonl"),
            origin: origin.map(String::from),
        }
    }

    fn catalog(running: Vec<RunningAgent>, recent: Vec<RecentFolder>) -> Catalog {
        Catalog {
            home: "/home/me".into(),
            probe: None,
            running,
            recent,
        }
    }

    fn busy() -> Catalog {
        catalog(
            vec![
                running(
                    "app-1",
                    "/home/me/app",
                    "fix login bug",
                    Some("gitlab.com/team/app"),
                ),
                running("infra-1", "/home/me/infra", "terraform drift", None),
            ],
            vec![
                recent(
                    "/srv/work/api",
                    "review cli",
                    3 * DAY,
                    Some("gitlab.com/team/api"),
                ),
                recent("/home/me/scripts", "cron cleanup", 8 * DAY, None),
                recent("/home/me", "home chores", 2 * HOUR, None),
            ],
        )
    }

    fn start(cwd: &str, session: Option<(&str, &str)>) -> Decision {
        Decision::Start {
            name: name_for_folder(cwd),
            cwd: cwd.into(),
            session: session.map(|(file, title)| Session {
                file: file.into(),
                title: title.into(),
            }),
        }
    }

    fn attach_to(name: &str) -> Decision {
        Decision::Attach { name: name.into() }
    }

    #[test]
    fn explicit_name_and_cwd_win_over_every_rule() {
        let catalog = busy();
        let named = |name, cwd| Explicit {
            name: Some(name),
            cwd,
        };
        assert_eq!(
            choose(&catalog, None, named("infra-1", None)),
            Choice::Open(attach_to("infra-1"))
        );
        assert_eq!(
            choose(&catalog, None, named("review", Some("~/scripts"))),
            Choice::Open(Decision::Start {
                name: "review".into(),
                cwd: "/home/me/scripts".into(),
                session: Some(Session {
                    file: "/home/me/scripts/session.jsonl".into(),
                    title: "cron cleanup".into(),
                }),
            })
        );
        let cwd_only = |cwd| Explicit {
            name: None,
            cwd: Some(cwd),
        };
        assert_eq!(
            choose(&catalog, None, cwd_only("~/app/")),
            Choice::Open(attach_to("app-1"))
        );
        assert_eq!(
            choose(&catalog, None, cwd_only("/srv/new")),
            Choice::Open(start("/srv/new", None))
        );
        assert_eq!(
            choose(&catalog, None, cwd_only("tools")),
            Choice::Open(start("/home/me/tools", None))
        );
    }

    #[test]
    fn a_matching_origin_prefers_a_running_agent_then_a_recent_folder() {
        let mut catalog = busy();
        catalog.recent.push(recent(
            "/srv/work/app",
            "old",
            DAY,
            Some("gitlab.com/team/app"),
        ));
        let origin = Some("gitlab.com/team/app");
        assert_eq!(
            choose(&catalog, origin, Explicit::default()),
            Choice::Open(attach_to("app-1"))
        );
        assert_eq!(
            choose(&catalog, Some("gitlab.com/team/api"), Explicit::default()),
            Choice::Open(start(
                "/srv/work/api",
                Some(("/srv/work/api/session.jsonl", "review cli"))
            ))
        );
        assert_eq!(
            choose(&catalog, Some("github.com/other/repo"), Explicit::default()),
            Choice::Ask
        );
    }

    #[test]
    fn the_same_folder_under_home_attaches_or_continues_or_starts() {
        let mut catalog = busy();
        catalog.probe = Some("/home/me/infra".into());
        assert_eq!(
            choose(&catalog, None, Explicit::default()),
            Choice::Open(attach_to("infra-1"))
        );
        catalog.probe = Some("/home/me/scripts".into());
        assert_eq!(
            choose(&catalog, None, Explicit::default()),
            Choice::Open(start(
                "/home/me/scripts",
                Some(("/home/me/scripts/session.jsonl", "cron cleanup"))
            ))
        );
        catalog.probe = Some("/home/me/fresh".into());
        assert_eq!(
            choose(&catalog, None, Explicit::default()),
            Choice::Open(start("/home/me/fresh", None))
        );
    }

    #[test]
    fn one_agent_attaches_an_empty_machine_starts_in_home_and_the_rest_ask() {
        let one = catalog(
            vec![running("app-1", "/home/me/app", "", None)],
            vec![recent("/srv/x", "", DAY, None)],
        );
        assert_eq!(
            choose(&one, None, Explicit::default()),
            Choice::Open(attach_to("app-1"))
        );
        let empty = catalog(Vec::new(), Vec::new());
        assert_eq!(
            choose(&empty, None, Explicit::default()),
            Choice::Open(start("/home/me", None))
        );
        let history = catalog(Vec::new(), vec![recent("/srv/x", "", DAY, None)]);
        assert_eq!(choose(&history, None, Explicit::default()), Choice::Ask);
        assert_eq!(choose(&busy(), None, Explicit::default()), Choice::Ask);
    }

    #[test]
    fn times_read_like_a_person_wrote_them() {
        assert_eq!(uptime(NOW, NOW - 2 * HOUR - 14 * MINUTE), "up 2h 14m");
        assert_eq!(uptime(NOW, NOW - 20 * MINUTE), "up 20m");
        assert_eq!(uptime(NOW, NOW - 3 * DAY - 5 * HOUR), "up 3d 5h");
        assert_eq!(uptime(NOW, NOW - 10), "up <1m");
        assert_eq!(uptime(NOW, NOW + 50), "up <1m");
        assert_eq!(ago(NOW, NOW - 5), "just now");
        assert_eq!(ago(NOW, NOW - 12 * MINUTE), "12m ago");
        assert_eq!(ago(NOW, NOW - 2 * HOUR), "2h ago");
        assert_eq!(ago(NOW, NOW - 30 * HOUR), "yesterday");
        assert_eq!(ago(NOW, NOW - 3 * DAY), "3 days ago");
        assert_eq!(ago(NOW, NOW - 9 * DAY), "last week");
        let march_15 = 1_773_576_000;
        assert!(ago(march_15 + 30 * DAY, march_15).starts_with("Mar 1"));
        assert!(ago(march_15 + 400 * DAY, march_15).ends_with(" 2026"));
    }

    fn texts(view: &View) -> Vec<String> {
        view.lines
            .iter()
            .map(|line| line.text.trim_end().to_string())
            .collect()
    }

    #[test]
    fn wide_characters_take_two_columns_in_the_list() {
        let catalog = catalog(
            Vec::new(),
            vec![
                recent(
                    "/home/me/日本語",
                    &"長いセッションの題名".repeat(8),
                    DAY,
                    None,
                ),
                recent("/home/me/api", "review cli", DAY, None),
            ],
        );
        let view = Picker::new(&catalog, "vm", NOW).view(SIZE);
        let lines = texts(&view);
        let detail = |line: &str| line.split('"').next().map(UnicodeWidthStr::width);
        assert_eq!(detail(&lines[2]), detail(&lines[3]));
        let drawn = String::from_utf8(Screen::default().draw(&view, 40)).unwrap();
        for line in drawn.split("\r\n") {
            let text = line
                .replace("\x1b[1m", "")
                .replace("\x1b[2m", "")
                .replace("\x1b[0m", "");
            let text = text.split("\x1b[?25").next().unwrap_or_default();
            assert!(text.width() <= 39, "{text}");
        }
    }

    #[test]
    fn the_list_groups_running_agents_and_recent_folders() {
        let catalog = busy();
        let picker = Picker::new(&catalog, "vm", NOW);
        let view = picker.view(SIZE);
        let lines = texts(&view);
        assert!(
            lines[0].starts_with("cloudthinker remote · vm")
                && lines[0].ends_with("type to filter")
        );
        assert_eq!(
            lines[1..].to_vec(),
            [
                " Running",
                " ▸ ● ~/app            \"fix login bug\" · up 2h 14m",
                "   ● ~/infra          \"terraform drift\" · up 2h 14m",
                " Recent folders",
                "   /srv/work/api      \"review cli\" · 3 days ago",
                "   ~/scripts          \"cron cleanup\" · last week",
                "   ~                  \"home chores\" · 2h ago",
                "   Other folder…      type a path · tab completes",
                " ↑↓ choose · enter open · n new session · esc cancel",
            ]
        );
        assert_eq!(view.lines[2].style, Style::Strong);
        assert_eq!(view.cursor, None);
    }

    #[test]
    fn typing_filters_by_folder_and_title() {
        let catalog = busy();
        let mut picker = Picker::new(&catalog, "vm", NOW);
        assert_eq!(picker.press(&decode(b"TERRA")), Action::Redraw);
        assert_eq!(picker.rows(), [Row::Running(1), Row::Other]);
        assert!(texts(&picker.view(SIZE))[0].ends_with("filter: TERRA"));
        picker.press(&[Key::Backspace; 5]);
        picker.press(&decode(b"scr"));
        assert_eq!(picker.rows(), [Row::Recent(1), Row::Other]);
        assert_eq!(
            picker.press(&[Key::Enter]),
            Action::Done(start(
                "/home/me/scripts",
                Some(("/home/me/scripts/session.jsonl", "cron cleanup"))
            ))
        );
    }

    #[test]
    fn arrows_enter_n_and_esc_choose_or_cancel() {
        let catalog = busy();
        let mut picker = Picker::new(&catalog, "vm", NOW);
        assert_eq!(
            picker.press(&[Key::Up, Key::Enter]),
            Action::Done(attach_to("app-1"))
        );
        let mut picker = Picker::new(&catalog, "vm", NOW);
        assert_eq!(picker.press(&[Key::Char('n')]), Action::Redraw);
        assert_eq!(picker.filter, "n");
        let mut picker = Picker::new(&catalog, "vm", NOW);
        picker.press(&[Key::Down, Key::Down, Key::Down]);
        assert_eq!(
            picker.press(&[Key::Char('n')]),
            Action::Done(start("/home/me/scripts", None))
        );
        let mut picker = Picker::new(&catalog, "vm", NOW);
        picker.press(&[Key::Down; 9]);
        assert_eq!(picker.press(&[Key::Up]), Action::Redraw);
        assert_eq!(
            picker.press(&[Key::Enter]),
            Action::Done(start(
                "/home/me",
                Some(("/home/me/session.jsonl", "home chores"))
            ))
        );
        let mut picker = Picker::new(&catalog, "vm", NOW);
        assert_eq!(picker.press(&decode(b"x\x1b")), Action::Cancel);
        let mut picker = Picker::new(&catalog, "vm", NOW);
        assert_eq!(picker.press(&[Key::Interrupt]), Action::Cancel);
    }

    #[test]
    fn other_folder_takes_a_path_with_tab_completion() {
        let catalog = busy();
        let mut picker = Picker::new(&catalog, "vm", NOW);
        picker.press(&decode(b"~/pro"));
        assert_eq!(picker.rows(), [Row::Other]);
        assert_eq!(picker.press(&[Key::Enter]), Action::Redraw);
        assert_eq!(picker.press(&[Key::Tab]), Action::Complete("~/pro".into()));
        for in_filter in [true, false] {
            let mut long = Picker::new(&catalog, "vm", NOW);
            long.press(&decode(b"~/pro"));
            if in_filter {
                long.press(&decode(&vec![b'a'; MAX_PATH * 2]));
                long.press(&[Key::Enter]);
            } else {
                long.press(&[Key::Enter]);
                long.press(&decode(&vec![b'a'; MAX_PATH * 2]));
            }
            let Action::Complete(typed) = long.press(&[Key::Tab]) else {
                panic!("tab completes the typed path");
            };
            assert_eq!(typed.len(), MAX_PATH);
        }
        picker.completions(vec!["~/projects/".into(), "~/prototypes/".into()]);
        let view = picker.view(SIZE);
        assert_eq!(
            texts(&view)[1..].to_vec(),
            [
                " Folder on vm: ~/pro",
                "   ~/projects/",
                "   ~/prototypes/",
                " tab completes · enter start · esc back",
            ]
        );
        assert_eq!(view.cursor, Some((1, 20)));
        picker.press(&decode(b"j"));
        picker.completions(vec!["~/projects/".into()]);
        assert_eq!(texts(&picker.view(SIZE))[1], " Folder on vm: ~/projects/");
        let many: Vec<String> = (0..12)
            .map(|index| format!("~/projects/p{index}/"))
            .collect();
        picker.completions(many);
        let lines = texts(&picker.view(SIZE));
        assert_eq!(lines[1], " Folder on vm: ~/projects/p");
        assert_eq!(lines.len(), 3 + MAX_MATCHES);
        picker.press(&decode(b"1"));
        assert_eq!(
            picker.press(&[Key::Enter]),
            Action::Done(start("/home/me/projects/p1", None))
        );
        let mut picker = Picker::new(&catalog, "vm", NOW);
        picker.press(&[Key::Down; 9]);
        picker.press(&[Key::Enter]);
        assert_eq!(picker.press(&[Key::Tab]), Action::Complete("~/".into()));
        assert_eq!(picker.press(&[Key::Escape]), Action::Redraw);
        assert!(matches!(picker.mode, Mode::List));
    }

    #[test]
    fn raw_bytes_decode_into_picker_keys() {
        assert_eq!(
            decode(b"\x1b[A\x1bOB\x1b[1;5A\rab\x7f\t\x03\x1b"),
            [
                Key::Up,
                Key::Down,
                Key::Up,
                Key::Enter,
                Key::Char('a'),
                Key::Char('b'),
                Key::Backspace,
                Key::Tab,
                Key::Interrupt,
                Key::Escape,
            ]
        );
        assert_eq!(decode("é".as_bytes()), [Key::Char('é')]);
        assert!(decode(b"\x1b[C\x1b[2").is_empty());
    }

    #[test]
    fn a_tall_list_keeps_the_header_the_footer_and_the_selection() {
        let folders = (0..8)
            .map(|index| recent(&format!("/srv/f{index}"), "", DAY * 3, None))
            .collect();
        let catalog = catalog(Vec::new(), folders);
        let mut picker = Picker::new(&catalog, "vm", NOW);
        picker.press(&[Key::Down; 7]);
        let size = Size { rows: 7, cols: 80 };
        let lines = texts(&picker.view(size));
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("cloudthinker remote"));
        assert!(lines[5].starts_with(" ↑↓ choose"));
        assert!(lines.iter().any(|line| line.starts_with(" ▸ /srv/f7")));
    }

    #[test]
    fn the_screen_redraws_in_place_and_clears_on_exit() {
        let mut screen = Screen::default();
        let view = View {
            lines: vec![dim("a"), dim("b"), dim("c")],
            cursor: None,
        };
        let first = screen.draw(&view, 80);
        assert!(first.starts_with(b"\x1b[2ma"));
        let second = screen.draw(&view, 80);
        assert!(second.starts_with(b"\r\x1b[2A\x1b[J"));
        let typing = View {
            lines: vec![dim("a"), dim("b: xy"), dim("c")],
            cursor: Some((1, 5)),
        };
        assert!(
            screen
                .draw(&typing, 80)
                .ends_with(b"\x1b[1A\r\x1b[5C\x1b[?25h")
        );
        assert_eq!(screen.clear(), b"\r\x1b[1A\x1b[J\x1b[?25h");
        assert_eq!(screen.clear(), b"\x1b[?25h");
    }

    #[test]
    fn the_common_prefix_stops_at_the_first_difference() {
        let entries = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            common_prefix(&entries(&["~/app/", "~/apps/"])),
            Some("~/app")
        );
        assert_eq!(common_prefix(&entries(&["~/a/"])), Some("~/a/"));
        assert_eq!(common_prefix(&entries(&["~/éa", "~/éb"])), Some("~/é"));
        assert_eq!(common_prefix(&[]), None);
    }
}
