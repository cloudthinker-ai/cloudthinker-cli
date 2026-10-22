use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Local};
use owo_colors::{AnsiColors, OwoColorize};
use uuid::Uuid;

use super::terminal_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerActivity {
    Command,
    FileRead,
    FileEdit,
    Internal,
}

#[derive(Debug, Clone, Copy)]
pub enum ConsoleEvent<'a> {
    Online,
    ConnectionLost {
        detail: &'a str,
    },
    RunStarted {
        run: Uuid,
    },
    OperationStarted {
        run: Uuid,
        operation: Option<Uuid>,
        kind: &'a str,
    },
    OperationFinished {
        run: Uuid,
        operation: Option<Uuid>,
        kind: &'a str,
        activity: WorkerActivity,
        outcome: &'a str,
        code: Option<&'a str>,
        duration_ms: u128,
    },
    RunFinished {
        run: Uuid,
    },
    RunStopped {
        run: Uuid,
        reason: &'a str,
    },
    Stopping {
        running: usize,
    },
    Cancelling,
    Stopped,
    Note {
        text: &'a str,
    },
    Warning {
        text: &'a str,
    },
    Detail {
        text: &'a str,
    },
    Silent,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsoleStyle {
    pub color: bool,
    pub terminal: bool,
}

#[derive(Default)]
struct Count {
    total: u32,
    failed: u32,
    timed_out: u32,
    cancelled: u32,
}

impl Count {
    fn add(&mut self, outcome: &str, code: Option<&str>) {
        self.total += 1;
        match (outcome, code) {
            ("cancelled", _) => self.cancelled += 1,
            ("failed", Some("TIMEOUT")) => self.timed_out += 1,
            ("failed", _) => self.failed += 1,
            _ => {}
        }
    }

    fn render(&self, singular: &str, plural: &str) -> Option<String> {
        if self.total == 0 {
            return None;
        }
        let noun = if self.total == 1 { singular } else { plural };
        let problems: Vec<String> = [
            (self.failed, "failed"),
            (self.timed_out, "timed out"),
            (self.cancelled, "cancelled"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, word)| format!("{count} {word}"))
        .collect();
        Some(if problems.is_empty() {
            format!("{} {noun}", self.total)
        } else {
            format!("{} {noun} ({})", self.total, problems.join(", "))
        })
    }

    fn problems(&self) -> u32 {
        self.failed + self.timed_out
    }
}

struct Tally {
    started: DateTime<Local>,
    commands: Count,
    reads: Count,
    edits: Count,
    other_failed: u32,
}

impl Tally {
    fn new(started: DateTime<Local>) -> Self {
        Self {
            started,
            commands: Count::default(),
            reads: Count::default(),
            edits: Count::default(),
            other_failed: 0,
        }
    }

    fn summary(&self) -> String {
        let mut parts: Vec<String> = [
            self.commands.render("command", "commands"),
            self.reads.render("file read", "file reads"),
            self.edits.render("file edit", "file edits"),
        ]
        .into_iter()
        .flatten()
        .collect();
        if self.other_failed > 0 {
            let noun = if self.other_failed == 1 {
                "other step"
            } else {
                "other steps"
            };
            parts.push(format!("{} {noun} failed", self.other_failed));
        }
        parts.join(", ")
    }

    fn has_problems(&self) -> bool {
        self.commands.problems() + self.reads.problems() + self.edits.problems() + self.other_failed
            > 0
    }
}

#[derive(Clone, Copy)]
enum Mark {
    None,
    Done,
    Failed,
    Warn,
    Start,
    Online,
    Detail,
}

pub struct WorkerConsole {
    style: ConsoleStyle,
    verbose: bool,
    offline: bool,
    online_once: bool,
    runs: HashMap<Uuid, Tally>,
}

pub struct WorkerHeader<'a> {
    pub name: &'a str,
    pub folder: &'a Path,
    pub label: Option<&'a str>,
    pub server: &'a str,
    pub concurrency: u16,
    pub version: &'a str,
}

pub fn short_id(id: Uuid) -> String {
    id.simple().to_string()[..8].to_owned()
}

pub fn human_duration(ms: u128) -> String {
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..10_000 => format!("{:.1}s", ms as f64 / 1_000.0),
        10_000..60_000 => format!("{}s", ms / 1_000),
        60_000..3_600_000 => format!("{}m {:02}s", ms / 60_000, (ms / 1_000) % 60),
        _ => format!("{}h {:02}m", ms / 3_600_000, (ms / 60_000) % 60),
    }
}

fn display_folder(folder: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let shown = match home
        .as_deref()
        .and_then(|home| folder.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => folder.display().to_string(),
    };
    terminal_text(&shown)
}

pub fn worker_header_lines(header: &WorkerHeader<'_>, style: ConsoleStyle) -> Vec<String> {
    let title = if style.color {
        format!(
            "{} {}",
            "CloudThinker Worker".bold(),
            format!("· CLI {}", header.version).dimmed()
        )
    } else {
        format!("CloudThinker Worker · CLI {}", header.version)
    };
    let folder = match header.label {
        Some(label) => format!(
            "{} ({})",
            display_folder(header.folder),
            terminal_text(label)
        ),
        None => display_folder(header.folder),
    };
    let row = |key: &str, value: String| {
        if style.color {
            format!("  {}  {value}", format!("{key:<6}").dimmed())
        } else {
            format!("  {key:<6}  {value}")
        }
    };
    let warning =
        "Shell commands run as your user, with no sandbox. File tools stay in this folder.";
    let warning = if style.color {
        format!("  {} {warning}", "!".color(AnsiColors::Yellow).bold())
    } else {
        format!("  ! {warning}")
    };
    let stop = if style.color {
        format!("  Press {} to stop.", "Ctrl-C".bold())
    } else {
        "  Press Ctrl-C to stop.".to_owned()
    };
    vec![
        title,
        row("Name", terminal_text(header.name)),
        row("Folder", folder),
        row("Server", terminal_text(header.server)),
        row("Runs", format!("up to {} at once", header.concurrency)),
        String::new(),
        warning,
        stop,
        String::new(),
    ]
}

fn command_noun(activity: WorkerActivity) -> &'static str {
    match activity {
        WorkerActivity::Command => "a shell command",
        WorkerActivity::FileEdit => "a file edit",
        WorkerActivity::FileRead => "a file read",
        WorkerActivity::Internal => "a step",
    }
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}

impl WorkerConsole {
    pub fn new(style: ConsoleStyle, verbose: bool) -> Self {
        Self {
            style,
            verbose,
            offline: false,
            online_once: false,
            runs: HashMap::new(),
        }
    }

    fn line(&self, now: DateTime<Local>, mark: Mark, text: &str) -> String {
        let time = if self.style.terminal {
            now.format("%H:%M:%S").to_string()
        } else {
            now.format("%Y-%m-%d %H:%M:%S").to_string()
        };
        let (symbol, color) = match mark {
            Mark::None => (" ", None),
            Mark::Done => ("✓", Some(AnsiColors::Green)),
            Mark::Failed => ("✗", Some(AnsiColors::Red)),
            Mark::Warn => ("!", Some(AnsiColors::Yellow)),
            Mark::Start => ("→", Some(AnsiColors::Cyan)),
            Mark::Online => ("●", Some(AnsiColors::Green)),
            Mark::Detail => ("·", None),
        };
        if !self.style.color {
            return format!("{time}  {symbol} {text}");
        }
        let symbol = match color {
            Some(color) => symbol.color(color).bold().to_string(),
            None => symbol.dimmed().to_string(),
        };
        if matches!(mark, Mark::Detail) {
            format!("{}  {symbol} {}", time.dimmed(), text.dimmed())
        } else {
            format!("{}  {symbol} {text}", time.dimmed())
        }
    }

    fn after_interrupt(&self, mut lines: Vec<String>) -> Vec<String> {
        if self.style.terminal {
            lines.insert(0, String::new());
        }
        lines
    }

    pub fn handle(&mut self, now: DateTime<Local>, event: ConsoleEvent<'_>) -> Vec<String> {
        let mut lines = Vec::new();
        match event {
            ConsoleEvent::Online => {
                let text = if self.offline {
                    "Reconnected."
                } else if self.online_once {
                    "Online again."
                } else {
                    "Online. Waiting for work."
                };
                self.offline = false;
                self.online_once = true;
                lines.push(self.line(now, Mark::Online, text));
            }
            ConsoleEvent::ConnectionLost { detail } => {
                if !self.offline {
                    self.offline = true;
                    lines.push(self.line(now, Mark::Warn, "Connection lost, reconnecting…"));
                }
                if self.verbose {
                    lines.push(self.line(now, Mark::Detail, &terminal_text(detail)));
                }
            }
            ConsoleEvent::RunStarted { run } => {
                self.runs.insert(run, Tally::new(now));
                lines.push(self.line(now, Mark::Start, &format!("Run {} started", short_id(run))));
            }
            ConsoleEvent::OperationStarted {
                run,
                operation,
                kind,
            } => {
                if self.verbose {
                    lines.push(self.line(
                        now,
                        Mark::Detail,
                        &format!("{} {kind} {} started", short_id(run), op_id(operation)),
                    ));
                }
            }
            ConsoleEvent::OperationFinished {
                run,
                operation,
                kind,
                activity,
                outcome,
                code,
                duration_ms,
            } => {
                if let Some(tally) = self.runs.get_mut(&run) {
                    match activity {
                        WorkerActivity::Command => tally.commands.add(outcome, code),
                        WorkerActivity::FileRead => tally.reads.add(outcome, code),
                        WorkerActivity::FileEdit => tally.edits.add(outcome, code),
                        WorkerActivity::Internal if outcome == "failed" => tally.other_failed += 1,
                        WorkerActivity::Internal => {}
                    }
                }
                let shown = matches!(activity, WorkerActivity::Command | WorkerActivity::FileEdit);
                if shown && outcome == "failed" {
                    let noun = command_noun(activity);
                    let duration = human_duration(duration_ms);
                    let text = match code {
                        Some("TIMEOUT") => format!("{noun} timed out after {duration}"),
                        Some(code) => {
                            format!("{noun} failed after {duration} ({})", terminal_text(code))
                        }
                        None => format!("{noun} failed after {duration}"),
                    };
                    lines.push(self.line(
                        now,
                        Mark::Failed,
                        &format!("Run {}: {text}", short_id(run)),
                    ));
                }
                if self.verbose {
                    let result = match code {
                        Some(code) => terminal_text(code),
                        None => "done".to_owned(),
                    };
                    lines.push(self.line(
                        now,
                        Mark::Detail,
                        &format!(
                            "{} {kind} {} {result} in {}",
                            short_id(run),
                            op_id(operation),
                            human_duration(duration_ms)
                        ),
                    ));
                }
            }
            ConsoleEvent::RunFinished { run } => {
                let tally = self.runs.remove(&run).unwrap_or_else(|| Tally::new(now));
                let elapsed = (now - tally.started).num_milliseconds().max(0);
                let mut text = format!(
                    "Run {} done in {}",
                    short_id(run),
                    human_duration(u128::try_from(elapsed).unwrap_or(0))
                );
                let summary = tally.summary();
                if !summary.is_empty() {
                    text.push_str(" · ");
                    text.push_str(&summary);
                }
                let mark = if tally.has_problems() {
                    Mark::Warn
                } else {
                    Mark::Done
                };
                lines.push(self.line(now, mark, &text));
            }
            ConsoleEvent::RunStopped { run, reason } => {
                self.runs.remove(&run);
                lines.push(self.line(
                    now,
                    Mark::Failed,
                    &format!(
                        "Run {} stopped: {}. Check the conversation in CloudThinker.",
                        short_id(run),
                        terminal_text(reason)
                    ),
                ));
            }
            ConsoleEvent::Stopping { running } => {
                let text = if running == 0 {
                    "Stopping… nothing running.".to_owned()
                } else {
                    format!(
                        "Stopping. Waiting for {} to finish; press Ctrl-C again to cancel.",
                        plural(running, "task", "tasks")
                    )
                };
                lines = self.after_interrupt(vec![self.line(now, Mark::None, &text)]);
            }
            ConsoleEvent::Cancelling => {
                lines = self.after_interrupt(vec![self.line(
                    now,
                    Mark::Warn,
                    "Cancelling running tasks.",
                )]);
            }
            ConsoleEvent::Stopped => lines.push(self.line(now, Mark::None, "Stopped.")),
            ConsoleEvent::Note { text } => {
                lines.push(self.line(now, Mark::None, &terminal_text(text)));
            }
            ConsoleEvent::Warning { text } => {
                lines.push(self.line(now, Mark::Warn, &terminal_text(text)));
            }
            ConsoleEvent::Detail { text } => {
                if self.verbose {
                    lines.push(self.line(now, Mark::Detail, &terminal_text(text)));
                }
            }
            ConsoleEvent::Silent => {}
        }
        lines
    }
}

fn op_id(operation: Option<Uuid>) -> String {
    operation.map_or_else(|| "-".to_owned(), short_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const PLAIN: ConsoleStyle = ConsoleStyle {
        color: false,
        terminal: true,
    };

    fn at(hour: u32, minute: u32, second: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2026, 10, 22, hour, minute, second)
            .unwrap()
    }

    fn run_id() -> Uuid {
        Uuid::parse_str("e1d607f2-14f4-4a4f-a125-098562a5b614").unwrap()
    }

    fn op(id: &str) -> Option<Uuid> {
        Some(Uuid::parse_str(id).unwrap())
    }

    fn finished<'a>(
        operation: &str,
        kind: &'a str,
        activity: WorkerActivity,
        code: Option<&'a str>,
        duration_ms: u128,
    ) -> ConsoleEvent<'a> {
        ConsoleEvent::OperationFinished {
            run: run_id(),
            operation: op(operation),
            kind,
            activity,
            outcome: match code {
                None => "succeeded",
                Some("CANCELLED") => "cancelled",
                Some(_) => "failed",
            },
            code,
            duration_ms,
        }
    }

    fn session(console: &mut WorkerConsole) -> Vec<String> {
        let events = [
            (at(16, 42, 43), ConsoleEvent::Online),
            (at(16, 42, 43), ConsoleEvent::RunStarted { run: run_id() }),
            (
                at(16, 42, 45),
                ConsoleEvent::OperationStarted {
                    run: run_id(),
                    operation: op("6027601d-3d67-42f6-afad-475e735cb110"),
                    kind: "file_operation",
                },
            ),
            (
                at(16, 42, 45),
                finished(
                    "6027601d-3d67-42f6-afad-475e735cb110",
                    "file_operation",
                    WorkerActivity::FileEdit,
                    None,
                    5,
                ),
            ),
            (
                at(16, 42, 46),
                finished(
                    "04432015-0000-4000-8000-000000000000",
                    "file_content",
                    WorkerActivity::FileRead,
                    None,
                    0,
                ),
            ),
            (
                at(16, 42, 48),
                finished(
                    "b8f377d8-0000-4000-8000-000000000000",
                    "script",
                    WorkerActivity::Command,
                    None,
                    20,
                ),
            ),
            (
                at(16, 42, 49),
                finished(
                    "a6f4a7c3-0000-4000-8000-000000000000",
                    "script",
                    WorkerActivity::Command,
                    Some("TIMEOUT"),
                    107,
                ),
            ),
            (
                at(16, 42, 51),
                finished(
                    "6489bed2-0000-4000-8000-000000000000",
                    "flush_output",
                    WorkerActivity::Internal,
                    None,
                    751,
                ),
            ),
            (
                at(16, 42, 53),
                finished(
                    "2b88ae3b-0000-4000-8000-000000000000",
                    "background",
                    WorkerActivity::Command,
                    None,
                    28,
                ),
            ),
            (at(16, 42, 55), ConsoleEvent::RunFinished { run: run_id() }),
            (
                at(16, 48, 30),
                ConsoleEvent::ConnectionLost {
                    detail: "Lost the connection to CloudThinker (transport error: worker transport retry deadline exceeded); reconnecting in 1s.",
                },
            ),
            (
                at(16, 48, 31),
                ConsoleEvent::ConnectionLost {
                    detail: "Lost the connection to CloudThinker (transport error: connection refused); reconnecting in 2s.",
                },
            ),
            (at(16, 48, 32), ConsoleEvent::Online),
            (at(16, 50, 53), ConsoleEvent::Stopping { running: 0 }),
            (at(16, 50, 53), ConsoleEvent::Silent),
            (at(16, 50, 54), ConsoleEvent::Stopped),
        ];
        events
            .into_iter()
            .flat_map(|(now, event)| console.handle(now, event))
            .collect()
    }

    #[test]
    fn worker_header_names_the_worker_folder_server_and_how_to_stop() {
        let header = WorkerHeader {
            name: "outpost\u{1b}]0;x",
            folder: Path::new("/srv/projects/outpost"),
            label: Some("api"),
            server: "https://app.cloudthinker.io",
            concurrency: 4,
            version: "0.9.0",
        };
        assert_eq!(
            worker_header_lines(&header, PLAIN).join("\n"),
            "CloudThinker Worker · CLI 0.9.0
  Name    outpost]0;x
  Folder  /srv/projects/outpost (api)
  Server  https://app.cloudthinker.io
  Runs    up to 4 at once

  ! Shell commands run as your user, with no sandbox. File tools stay in this folder.
  Press Ctrl-C to stop.
"
        );
    }

    #[test]
    fn a_run_prints_one_start_line_one_summary_and_its_failures() {
        let mut console = WorkerConsole::new(PLAIN, false);
        assert_eq!(
            session(&mut console).join("\n"),
            "16:42:43  ● Online. Waiting for work.
16:42:43  → Run e1d607f2 started
16:42:49  ✗ Run e1d607f2: a shell command timed out after 107 ms
16:42:55  ! Run e1d607f2 done in 12s · 3 commands (1 timed out), 1 file read, 1 file edit
16:48:30  ! Connection lost, reconnecting…
16:48:32  ● Reconnected.

16:50:53    Stopping… nothing running.
16:50:54    Stopped."
        );
    }

    #[test]
    fn verbose_mode_adds_every_operation_and_connection_detail() {
        let mut console = WorkerConsole::new(PLAIN, true);
        let lines = session(&mut console);
        for expected in [
            "16:42:45  · e1d607f2 file_operation 6027601d started",
            "16:42:45  · e1d607f2 file_operation 6027601d done in 5 ms",
            "16:42:49  · e1d607f2 script a6f4a7c3 TIMEOUT in 107 ms",
            "16:42:51  · e1d607f2 flush_output 6489bed2 done in 751 ms",
            "16:48:31  · Lost the connection to CloudThinker (transport error: connection refused); reconnecting in 2s.",
        ] {
            assert!(
                lines.iter().any(|line| line == expected),
                "missing {expected} in {lines:#?}"
            );
        }
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("Connection lost"))
                .count(),
            1
        );
    }

    #[test]
    fn a_clean_run_and_a_failed_edit_and_an_interrupt_with_work_running() {
        let mut console = WorkerConsole::new(
            ConsoleStyle {
                color: false,
                terminal: false,
            },
            false,
        );
        let mut lines = Vec::new();
        lines.extend(console.handle(at(9, 0, 0), ConsoleEvent::RunStarted { run: run_id() }));
        lines.extend(console.handle(
            at(9, 0, 1),
            finished(
                "b8f377d8-0000-4000-8000-000000000000",
                "file_operation",
                WorkerActivity::FileEdit,
                Some("PATH_OUTSIDE_WORKDIR"),
                3,
            ),
        ));
        lines.extend(console.handle(
            at(9, 0, 2),
            finished(
                "a6f4a7c3-0000-4000-8000-000000000000",
                "file_operation",
                WorkerActivity::FileRead,
                Some("NOT_FOUND"),
                1,
            ),
        ));
        lines.extend(console.handle(at(9, 1, 30), ConsoleEvent::RunFinished { run: run_id() }));
        lines.extend(console.handle(at(9, 2, 0), ConsoleEvent::RunStarted { run: run_id() }));
        lines.extend(console.handle(at(9, 2, 4), ConsoleEvent::RunFinished { run: run_id() }));
        lines.extend(console.handle(at(9, 3, 0), ConsoleEvent::Stopping { running: 2 }));
        lines.extend(console.handle(at(9, 3, 5), ConsoleEvent::Cancelling));
        assert_eq!(
            lines.join("\n"),
            "2026-10-22 09:00:00  → Run e1d607f2 started
2026-10-22 09:00:01  ✗ Run e1d607f2: a file edit failed after 3 ms (PATH_OUTSIDE_WORKDIR)
2026-10-22 09:01:30  ! Run e1d607f2 done in 1m 30s · 1 file read (1 failed), 1 file edit (1 failed)
2026-10-22 09:02:00  → Run e1d607f2 started
2026-10-22 09:02:04  ✓ Run e1d607f2 done in 4.0s
2026-10-22 09:03:00    Stopping. Waiting for 2 tasks to finish; press Ctrl-C again to cancel.
2026-10-22 09:03:05  ! Cancelling running tasks."
        );
    }

    #[test]
    fn a_run_whose_only_failure_is_a_file_read_is_not_marked_clean() {
        let mut console = WorkerConsole::new(PLAIN, false);
        let mut lines = Vec::new();
        lines.extend(console.handle(at(9, 0, 0), ConsoleEvent::RunStarted { run: run_id() }));
        lines.extend(console.handle(
            at(9, 0, 1),
            finished(
                "a6f4a7c3-0000-4000-8000-000000000000",
                "file_content",
                WorkerActivity::FileRead,
                Some("NOT_FOUND"),
                1,
            ),
        ));
        lines.extend(console.handle(at(9, 0, 4), ConsoleEvent::RunFinished { run: run_id() }));
        assert_eq!(
            lines.last().map(String::as_str),
            Some("09:00:04  ! Run e1d607f2 done in 4.0s · 1 file read (1 failed)")
        );
    }
}
