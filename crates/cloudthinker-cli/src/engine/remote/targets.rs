use std::path::PathBuf;

use cap_std::fs::Dir;
use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::{CtError, CtResult};

use super::paths::ROOT_ENV;

const FILE: &str = "targets";
const KEEP: usize = 10;
const LIMIT: u64 = 64 * 1024;
const VIA_PREFIX: &str = "-via\t";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub target: String,
    pub via: Option<String>,
}

pub fn latest() -> CtResult<Option<Saved>> {
    Ok(list()?.into_iter().next())
}

pub fn find(target: &str) -> CtResult<Option<Saved>> {
    Ok(list()?.into_iter().find(|saved| saved.target == target))
}

pub fn list() -> CtResult<Vec<Saved>> {
    read(&open()?)
}

pub fn remember(target: &str, via: Option<&str>) -> CtResult<()> {
    let dir = open()?;
    let saved = Saved {
        target: target.to_string(),
        via: via.map(str::to_string),
    };
    let list = put_first(read(&dir)?, saved);
    write_private(&dir, FILE, render(&list).as_bytes())
}

pub fn forget(target: &str) -> CtResult<bool> {
    let dir = open()?;
    let mut list = read(&dir)?;
    let before = list.len();
    list.retain(|saved| saved.target != target);
    if list.len() == before {
        return Ok(false);
    }
    write_private(&dir, FILE, render(&list).as_bytes())?;
    Ok(true)
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

fn read(dir: &Dir) -> CtResult<Vec<Saved>> {
    if !dir.exists(FILE) {
        return Ok(Vec::new());
    }
    let bytes = read_private(dir, FILE, LIMIT)?;
    Ok(parse(&String::from_utf8_lossy(&bytes)))
}

fn parse(text: &str) -> Vec<Saved> {
    let mut targets: Vec<Saved> = Vec::new();
    for saved in text.lines().map(str::trim).filter_map(parse_line) {
        if !targets.iter().any(|seen| seen.target == saved.target) {
            targets.push(saved);
        }
    }
    targets.truncate(KEEP);
    targets
}

fn parse_line(line: &str) -> Option<Saved> {
    if let Some(rest) = line.strip_prefix(VIA_PREFIX) {
        let (target, via) = rest.split_once('\t')?;
        let valid = !target.is_empty() && !target.starts_with('-') && !via.trim().is_empty();
        return valid.then(|| Saved {
            target: target.to_string(),
            via: Some(via.to_string()),
        });
    }
    (!line.is_empty() && !line.starts_with('-')).then(|| Saved {
        target: line.to_string(),
        via: None,
    })
}

fn put_first(mut targets: Vec<Saved>, saved: Saved) -> Vec<Saved> {
    targets.retain(|seen| seen.target != saved.target);
    targets.insert(0, saved);
    targets.truncate(KEEP);
    targets
}

fn render(targets: &[Saved]) -> String {
    targets
        .iter()
        .map(|saved| match &saved.via {
            Some(via) => format!("{VIA_PREFIX}{}\t{via}\n", saved.target),
            None => format!("{}\n", saved.target),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<Saved> {
        items
            .iter()
            .map(|item| Saved {
                target: (*item).to_string(),
                via: None,
            })
            .collect()
    }

    fn plain(target: &str) -> Saved {
        Saved {
            target: target.to_string(),
            via: None,
        }
    }

    #[test]
    fn the_newest_target_comes_first_and_the_list_keeps_ten() {
        let mut targets = Vec::new();
        for target in ["a", "b", "c", "a"] {
            targets = put_first(targets, plain(target));
        }
        assert_eq!(targets, list(&["a", "c", "b"]));
        for index in 0..12 {
            targets = put_first(targets, plain(&format!("host-{index}")));
        }
        assert_eq!(targets.len(), KEEP);
        assert_eq!(
            targets.first().map(|saved| saved.target.as_str()),
            Some("host-11")
        );
        assert_eq!(
            targets.last().map(|saved| saved.target.as_str()),
            Some("host-2")
        );
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
