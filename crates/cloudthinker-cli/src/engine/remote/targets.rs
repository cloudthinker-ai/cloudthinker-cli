use std::path::PathBuf;

use cap_std::fs::Dir;
use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::{CtError, CtResult};

use super::paths::ROOT_ENV;

const FILE: &str = "targets";
const KEEP: usize = 10;
const LIMIT: u64 = 64 * 1024;

pub fn latest() -> CtResult<Option<String>> {
    let dir = open()?;
    Ok(read(&dir)?.into_iter().next())
}

pub fn remember(target: &str) -> CtResult<()> {
    let dir = open()?;
    let list = put_first(read(&dir)?, target);
    write_private(&dir, FILE, render(&list).as_bytes())
}

fn open() -> CtResult<Dir> {
    let root = match std::env::var_os(ROOT_ENV) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => match std::env::var_os("HOME") {
            Some(home) if !home.is_empty() => {
                PathBuf::from(home).join(".cloudthinker").join("remote")
            }
            _ => return Err(CtError::Usage("HOME is not set".into())),
        },
    };
    private_directory(&root).map_err(|_| {
        CtError::Usage(format!(
            "{} must be a folder only you can open; fix it with: chmod 700 {}",
            root.display(),
            root.display()
        ))
    })
}

fn read(dir: &Dir) -> CtResult<Vec<String>> {
    if !dir.exists(FILE) {
        return Ok(Vec::new());
    }
    let bytes = read_private(dir, FILE, LIMIT)?;
    Ok(parse(&String::from_utf8_lossy(&bytes)))
}

fn parse(text: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in text.lines().map(str::trim) {
        if !line.is_empty() && !line.starts_with('-') && !targets.iter().any(|seen| seen == line) {
            targets.push(line.to_string());
        }
    }
    targets.truncate(KEEP);
    targets
}

fn put_first(mut targets: Vec<String>, target: &str) -> Vec<String> {
    targets.retain(|seen| seen != target);
    targets.insert(0, target.to_string());
    targets.truncate(KEEP);
    targets
}

fn render(targets: &[String]) -> String {
    targets.iter().map(|target| format!("{target}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn the_newest_target_comes_first_and_the_list_keeps_ten() {
        let mut targets = Vec::new();
        for target in ["a", "b", "c", "a"] {
            targets = put_first(targets, target);
        }
        assert_eq!(targets, list(&["a", "c", "b"]));
        for index in 0..12 {
            targets = put_first(targets, &format!("host-{index}"));
        }
        assert_eq!(targets.len(), KEEP);
        assert_eq!(targets.first().map(String::as_str), Some("host-11"));
        assert_eq!(targets.last().map(String::as_str), Some("host-2"));
    }

    #[test]
    fn the_file_round_trips_and_bad_lines_are_skipped() {
        let targets = list(&["me@vm", "workbox"]);
        assert_eq!(parse(&render(&targets)), targets);
        assert_eq!(
            parse("  vm \n\n-oProxyCommand=x\nvm\nother\n"),
            list(&["vm", "other"])
        );
        assert!(parse("").is_empty());
    }
}
