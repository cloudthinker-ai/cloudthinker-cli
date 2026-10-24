use std::io;

use console::{Key, Term};
use owo_colors::OwoColorize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Browser,
    Code,
}

const METHODS: [(Method, &str, &str); 2] = [
    (
        Method::Browser,
        "Log in with browser",
        "Recommended for most users",
    ),
    (
        Method::Code,
        "Log in with a code",
        "For SSH or remote sessions without a browser",
    ),
];

const KEYS_HINT: &str = "↑↓ select · ↵ confirm · esc cancel";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Select(usize),
    Choose(usize),
    Cancel,
    Stay,
}

fn next(key: &Key, selected: usize) -> Next {
    let last = METHODS.len() - 1;
    match key {
        Key::ArrowUp | Key::Char('k') => Next::Select(selected.saturating_sub(1)),
        Key::ArrowDown | Key::Char('j') | Key::Tab => Next::Select((selected + 1).min(last)),
        Key::Enter => Next::Choose(selected),
        Key::Escape | Key::CtrlC | Key::Char('q') => Next::Cancel,
        Key::Char(digit) => match digit.to_digit(10).map(|n| n as usize) {
            Some(n) if (1..=METHODS.len()).contains(&n) => Next::Choose(n - 1),
            _ => Next::Stay,
        },
        _ => Next::Stay,
    }
}

fn index_of(method: Method) -> usize {
    METHODS
        .iter()
        .position(|(candidate, _, _)| *candidate == method)
        .unwrap_or(0)
}

fn menu(selected: usize, color: bool) -> Vec<String> {
    let mut lines = Vec::with_capacity(METHODS.len() * 2 + 2);
    for (index, (_, label, detail)) in METHODS.iter().enumerate() {
        let number = index + 1;
        let (marker, label) = match (index == selected, color) {
            (true, true) => (
                "❯".cyan().bold().to_string(),
                label.cyan().bold().to_string(),
            ),
            (true, false) => ("❯".to_string(), (*label).to_string()),
            (false, _) => (" ".to_string(), (*label).to_string()),
        };
        lines.push(format!("{marker} {number} {label}"));
        lines.push(if color {
            format!("    {}", detail.dimmed())
        } else {
            format!("    {detail}")
        });
    }
    lines.push(String::new());
    lines.push(if color {
        KEYS_HINT.dimmed().to_string()
    } else {
        KEYS_HINT.to_string()
    });
    lines
}

fn header(color: bool) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let question = "How would you like to log in?";
    if color {
        format!(
            "\n{}  {}\n\n{}\n",
            "CloudThinker CLI".bold(),
            format!("v{version}").dimmed(),
            question.bold()
        )
    } else {
        format!("\nCloudThinker CLI  v{version}\n\n{question}\n")
    }
}

pub fn ask(suggested: Method) -> io::Result<Option<Method>> {
    let term = Term::stderr();
    let color = supports_color::on(supports_color::Stream::Stderr).is_some();
    term.write_line(&header(color))?;
    term.hide_cursor()?;
    let answer = choose(&term, index_of(suggested), color);
    let _ = term.show_cursor();
    let answer = answer?;
    if let Some(method) = answer {
        let label = METHODS[index_of(method)].1;
        term.write_line(&format!("❯ {label}\n"))?;
    }
    Ok(answer)
}

fn choose(term: &Term, mut selected: usize, color: bool) -> io::Result<Option<Method>> {
    loop {
        let lines = menu(selected, color);
        term.write_line(&lines.join("\n"))?;
        let outcome = loop {
            match next(&term.read_key_raw()?, selected) {
                Next::Stay => continue,
                outcome => break outcome,
            }
        };
        term.clear_last_lines(lines.len())?;
        match outcome {
            Next::Select(index) => selected = index,
            Next::Choose(index) => return Ok(Some(METHODS[index].0)),
            Next::Cancel | Next::Stay => return Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_move_within_the_list_and_digits_choose_directly() {
        let cases = [
            (Key::ArrowDown, 0, Next::Select(1)),
            (Key::ArrowDown, 1, Next::Select(1)),
            (Key::ArrowUp, 0, Next::Select(0)),
            (Key::ArrowUp, 1, Next::Select(0)),
            (Key::Enter, 1, Next::Choose(1)),
            (Key::Char('2'), 0, Next::Choose(1)),
            (Key::Char('3'), 0, Next::Stay),
            (Key::Char('0'), 0, Next::Stay),
            (Key::Escape, 0, Next::Cancel),
            (Key::CtrlC, 1, Next::Cancel),
        ];
        for (key, selected, expected) in cases {
            assert_eq!(next(&key, selected), expected, "{key:?} from {selected}");
        }
    }

    #[test]
    fn the_menu_marks_only_the_selected_method() {
        let lines = menu(index_of(Method::Code), false);

        assert_eq!(lines[0], "  1 Log in with browser");
        assert_eq!(lines[2], "❯ 2 Log in with a code");
        assert_eq!(lines.last().map(String::as_str), Some(KEYS_HINT));
    }
}
