use std::io::{BufRead, IsTerminal, Write};

use super::output;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Given,
    Ask,
    Refuse,
}

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn consent_gate(yes: bool, interactive: bool) -> Consent {
    match (yes, interactive) {
        (true, _) => Consent::Given,
        (false, true) => Consent::Ask,
        (false, false) => Consent::Refuse,
    }
}

pub fn ask(prompt: &str, untouched: &str, input: &mut impl BufRead) -> bool {
    eprint!("{prompt}");
    if let Err(error) = std::io::stderr().flush() {
        output::eprintln_error(&format!(
            "could not show the confirmation question: {error}; {untouched}"
        ));
        return false;
    }
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(_) => accepts(&line),
        Err(error) => {
            output::eprintln_error(&format!("could not read your answer: {error}; {untouched}"));
            false
        }
    }
}

fn accepts(line: &str) -> bool {
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{Consent, accepts, ask, consent_gate};

    #[test]
    fn asks_on_a_terminal_and_refuses_without_one() {
        assert_eq!(consent_gate(true, true), Consent::Given);
        assert_eq!(consent_gate(true, false), Consent::Given);
        assert_eq!(consent_gate(false, true), Consent::Ask);
        assert_eq!(consent_gate(false, false), Consent::Refuse);
    }

    #[test]
    fn only_an_explicit_yes_confirms() {
        for line in ["y\n", "Y", "yes\n", " Yes "] {
            assert!(accepts(line), "{line:?}");
        }
        for line in ["", "\n", "n", "no", "yep", "aws"] {
            assert!(!accepts(line), "{line:?}");
        }
        assert!(ask("", "", &mut Cursor::new("yes\n")));
        assert!(!ask("", "", &mut Cursor::new("\n")));
        assert!(!ask("", "", &mut Cursor::new("")));
    }
}
