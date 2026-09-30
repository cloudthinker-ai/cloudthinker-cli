use std::io::{IsTerminal, Read};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

pub const PROMPT_LIMIT_CHARS: usize = 50_000;
const READ_LIMIT_BYTES: usize = PROMPT_LIMIT_CHARS * 4 + 4;
const FIRST_BYTE_WAIT: Duration = Duration::from_secs(3);
const STDIN_PROMPT: &str = "-";

pub fn resolve_prompt(prompt: &str) -> Result<String, String> {
    let stdin_is_terminal = std::io::stdin().is_terminal();
    if prompt == STDIN_PROMPT {
        if stdin_is_terminal {
            return Err(
                "`-p -` reads the prompt from stdin; pipe or redirect the prompt into the command"
                    .into(),
            );
        }
        let data = read_stdin(None)?.unwrap_or_default();
        return prompt_from_stdin(&data);
    }
    if stdin_is_terminal {
        return Ok(prompt.to_string());
    }
    match read_stdin(Some(FIRST_BYTE_WAIT))? {
        Some(data) => combine(prompt, &data),
        None => {
            crate::engine::output::warn(&format!(
                "no input arrived on stdin within {}s; sending the prompt without it. Redirect stdin from /dev/null to skip this wait.",
                FIRST_BYTE_WAIT.as_secs()
            ));
            Ok(prompt.to_string())
        }
    }
}

fn prompt_from_stdin(data: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(data).trim().to_string();
    if text.is_empty() {
        return Err("`-p -` found no prompt on stdin".into());
    }
    within_limit(text)
}

fn combine(prompt: &str, data: &[u8]) -> Result<String, String> {
    let piped = String::from_utf8_lossy(data);
    let piped = piped.trim_end();
    if piped.trim().is_empty() {
        return Ok(prompt.to_string());
    }
    within_limit(format!("{prompt}\n\n<stdin>\n{piped}\n</stdin>"))
}

fn within_limit(prompt: String) -> Result<String, String> {
    let length = prompt.chars().count();
    if length > PROMPT_LIMIT_CHARS {
        return Err(format!(
            "the prompt with its piped input has {length} characters; the limit is {PROMPT_LIMIT_CHARS}. Send less input, for example with `tail -n 200`"
        ));
    }
    Ok(prompt)
}

fn read_stdin(first_byte_wait: Option<Duration>) -> Result<Option<Vec<u8>>, String> {
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut data = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut started = Some(started_tx);
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if let Some(started) = started.take() {
                        let _ = started.send(());
                    }
                    data.extend_from_slice(&buffer[..read]);
                    if data.len() > READ_LIMIT_BYTES {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = done_tx.send(Err(error));
                    return;
                }
            }
        }
        let _ = done_tx.send(Ok(data));
    });
    if let Some(wait) = first_byte_wait
        && let Err(RecvTimeoutError::Timeout) = started_rx.recv_timeout(wait)
    {
        return Ok(None);
    }
    match done_rx.recv() {
        Ok(Ok(data)) => Ok(Some(data)),
        Ok(Err(error)) => Err(format!("could not read stdin: {error}")),
        Err(_) => Err("could not read stdin".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piped_input_follows_the_prompt_inside_stdin_tags() {
        assert_eq!(
            combine("why?", b"line 1\nline 2\n").unwrap(),
            "why?\n\n<stdin>\nline 1\nline 2\n</stdin>"
        );
        assert_eq!(combine("why?", b" \n").unwrap(), "why?");
    }

    #[test]
    fn a_prompt_over_the_server_limit_is_refused_before_submission() {
        let error = combine("why?", "x".repeat(PROMPT_LIMIT_CHARS).as_bytes()).unwrap_err();
        assert!(error.contains("the limit is 50000"), "{error}");
        assert!(prompt_from_stdin(b"  \n").is_err());
        assert_eq!(prompt_from_stdin(b" hello \n").unwrap(), "hello");
    }
}
