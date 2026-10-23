use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::{CtError, CtResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::config::WorkdirIdentity;

pub const WORKTREE_CODE: &str = "EXECUTOR_WORKTREE_UNAVAILABLE";
pub const MAX_IDLE: Duration = Duration::from_secs(14 * 24 * 60 * 60);
pub const MAX_BRANCHES: usize = 200;
pub const WORKTREES_DIR: &str = "worktrees";
const RECORD_LIMIT: u64 = 16 * 1024;
const TOUCH_EVERY: Duration = Duration::from_secs(10 * 60);
const LOCAL_GIT_TIMEOUT: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
const MIN_GIT: (u32, u32) = (2, 38);
const BRANCH_PREFIX: &str = "cloudthinker/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub filesystem_id: Uuid,
    pub base_branch: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    NotGit,
    BranchMissing(String),
    Other(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Self::NotGit => "This folder is no longer a git repository. Restore it on the machine, or start a new conversation without a worktree.".into(),
            Self::BranchMissing(base) => format!(
                "Branch {base} was not found on this Worker. Fetch it on the machine, or pick another branch."
            ),
            Self::Other(reason) => format!(
                "The Worker could not create the worktree: {reason}. Check the folder on the machine, then send the message again."
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub filesystem_id: Uuid,
    pub branch: String,
    pub base: String,
    pub repository: PathBuf,
    pub created_at: u64,
    pub last_used: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Listed {
    #[serde(flatten)]
    pub record: Record,
    pub path: PathBuf,
    pub state: &'static str,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Pruned {
    pub filesystem_id: Uuid,
    pub branch: String,
    pub path: PathBuf,
    pub outcome: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryState {
    pub is_git: bool,
    pub head_branch: Option<String>,
    pub branches: Vec<String>,
    pub digest: String,
}

pub struct Worktrees {
    state: PathBuf,
    live: Mutex<HashMap<Uuid, Arc<WorkdirIdentity>>>,
    creating: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    touched: StdMutex<HashMap<Uuid, Instant>>,
}

impl Worktrees {
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.to_owned(),
            live: Mutex::new(HashMap::new()),
            creating: Mutex::new(HashMap::new()),
            touched: StdMutex::new(HashMap::new()),
        }
    }

    pub async fn resolve(
        &self,
        main: &Arc<WorkdirIdentity>,
        workspace: &Workspace,
    ) -> Result<Arc<WorkdirIdentity>, Failure> {
        if let Some(existing) = self.cached(workspace.filesystem_id).await {
            self.touch(workspace.filesystem_id);
            return Ok(existing);
        }
        let gate = self.gate(&main.path).await;
        let _creating = gate.lock().await;
        if let Some(existing) = self.cached(workspace.filesystem_id).await {
            self.touch(workspace.filesystem_id);
            return Ok(existing);
        }
        let (state, repository, workspace_copy) =
            (self.state.clone(), main.path.clone(), workspace.clone());
        let path =
            tokio::task::spawn_blocking(move || ensure(&state, &repository, &workspace_copy))
                .await
                .map_err(|_| Failure::Other("the git command was interrupted".into()))??;
        let rooted = main
            .rooted_at(&path)
            .map_err(|_| Failure::Other("the worktree folder is unavailable".into()))?;
        let rooted = Arc::new(rooted);
        self.live
            .lock()
            .await
            .insert(workspace.filesystem_id, rooted.clone());
        self.touch(workspace.filesystem_id);
        Ok(rooted)
    }

    async fn cached(&self, id: Uuid) -> Option<Arc<WorkdirIdentity>> {
        let mut live = self.live.lock().await;
        let existing = live.get(&id)?;
        if existing.revalidate().is_ok() {
            return Some(existing.clone());
        }
        live.remove(&id);
        None
    }

    async fn gate(&self, repository: &Path) -> Arc<Mutex<()>> {
        self.creating
            .lock()
            .await
            .entry(repository.to_owned())
            .or_default()
            .clone()
    }

    fn touch(&self, id: Uuid) {
        let due = match self.touched.lock() {
            Ok(mut touched) => {
                let due = touched
                    .get(&id)
                    .is_none_or(|last| last.elapsed() >= TOUCH_EVERY);
                if due {
                    touched.insert(id, Instant::now());
                }
                due
            }
            Err(_) => false,
        };
        if due {
            let state = self.state.clone();
            drop(tokio::task::spawn_blocking(move || {
                let _ = mark_used(&state, id);
            }));
        }
    }
}

fn now_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn short_id(id: Uuid) -> String {
    id.simple().to_string().chars().take(8).collect()
}

fn branch_name(id: Uuid) -> String {
    format!("{BRANCH_PREFIX}{}", short_id(id))
}

fn worktrees_dir(state: &Path) -> CtResult<cap_std::fs::Dir> {
    private_directory(&state.join(WORKTREES_DIR))
}

fn record_name(id: Uuid) -> String {
    format!("{id}.json")
}

fn read_record(dir: &cap_std::fs::Dir, name: &str) -> Option<Record> {
    let bytes = read_private(dir, name, RECORD_LIMIT).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_record(state: &Path, record: &Record) -> CtResult<()> {
    let dir = worktrees_dir(state)?;
    let bytes = serde_json::to_vec(record)
        .map_err(|_| CtError::Store("worktree record could not be encoded".into()))?;
    write_private(&dir, &record_name(record.filesystem_id), &bytes)
}

fn mark_used(state: &Path, id: Uuid) -> CtResult<()> {
    let dir = worktrees_dir(state)?;
    let Some(mut record) = read_record(&dir, &record_name(id)) else {
        return Ok(());
    };
    record.last_used = now_secs(SystemTime::now());
    write_record(state, &record)
}

struct GitOutput {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn git(dir: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"CLOUDTHINKER_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Some(GitOutput {
        ok: status.is_some_and(|status| status.success()),
        stdout,
        stderr,
    })
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args, LOCAL_GIT_TIMEOUT).is_some_and(|output| output.ok)
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    let output = git(dir, args, LOCAL_GIT_TIMEOUT)?;
    let line = output.stdout.trim();
    (output.ok && !line.is_empty()).then(|| line.to_owned())
}

fn git_version() -> Option<(u32, u32)> {
    let output = Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let version = text.split_whitespace().nth(2)?;
    let mut parts = version.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

pub fn git_supports_worktrees() -> bool {
    git_version().is_some_and(|version| version >= MIN_GIT)
}

fn is_toplevel(path: &Path) -> bool {
    git_line(path, &["rev-parse", "--show-toplevel"])
        .and_then(|top| PathBuf::from(top).canonicalize().ok())
        .zip(path.canonicalize().ok())
        .is_some_and(|(top, served)| top == served)
}

fn local_branch(repository: &Path, name: &str) -> bool {
    git_ok(
        repository,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}^{{commit}}"),
        ],
    )
}

fn remote_branch(repository: &Path, name: &str) -> bool {
    git_ok(
        repository,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{name}^{{commit}}"),
        ],
    )
}

fn start_point(repository: &Path, base: &str) -> Result<String, Failure> {
    if !git_ok(repository, &["check-ref-format", "--branch", base]) {
        return Err(Failure::BranchMissing(base.to_owned()));
    }
    if local_branch(repository, base) {
        return Ok(format!("refs/heads/{base}"));
    }
    let _ = git(
        repository,
        &["fetch", "--no-tags", "--quiet", "origin", base],
        FETCH_TIMEOUT,
    );
    if remote_branch(repository, base) {
        return Ok(format!("refs/remotes/origin/{base}"));
    }
    Err(Failure::BranchMissing(base.to_owned()))
}

fn ensure(state: &Path, repository: &Path, workspace: &Workspace) -> Result<PathBuf, Failure> {
    worktrees_dir(state)
        .map_err(|_| Failure::Other("the worker state directory is unavailable".into()))?;
    let target = state
        .join(WORKTREES_DIR)
        .join(workspace.filesystem_id.to_string());
    if !is_toplevel(repository) {
        return Err(Failure::NotGit);
    }
    let recorded = has_record(state, workspace.filesystem_id);
    if recorded && target.join(".git").exists() && is_toplevel(&target) {
        return Ok(target.canonicalize().unwrap_or(target));
    }
    let branch = branch_name(workspace.filesystem_id);
    let target_text = target.to_string_lossy().into_owned();
    if !recorded && std::fs::symlink_metadata(&target).is_ok() {
        discard(repository, &target)?;
    }
    let fresh = std::fs::symlink_metadata(&target).is_err();
    let _ = git(repository, &["worktree", "prune"], LOCAL_GIT_TIMEOUT);
    let output = if local_branch(repository, &branch) {
        git(
            repository,
            &["worktree", "add", "--quiet", &target_text, &branch],
            FETCH_TIMEOUT,
        )
    } else {
        let start = start_point(repository, &workspace.base_branch)?;
        git(
            repository,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-track",
                "-b",
                &branch,
                &target_text,
                &start,
            ],
            FETCH_TIMEOUT,
        )
    };
    match output {
        Some(output) if output.ok => {}
        Some(output) => {
            if fresh {
                let _ = discard(repository, &target);
            }
            return Err(Failure::Other(first_line(&output.stderr)));
        }
        None => {
            if fresh {
                let _ = discard(repository, &target);
            }
            return Err(Failure::Other("git did not finish".into()));
        }
    }
    let now = now_secs(SystemTime::now());
    let record = Record {
        filesystem_id: workspace.filesystem_id,
        branch,
        base: workspace.base_branch.clone(),
        repository: repository.to_owned(),
        created_at: now,
        last_used: now,
    };
    write_record(state, &record)
        .map_err(|_| Failure::Other("the worktree record could not be saved".into()))?;
    Ok(target.canonicalize().unwrap_or(target))
}

fn has_record(state: &Path, id: Uuid) -> bool {
    worktrees_dir(state).is_ok_and(|dir| dir.exists(record_name(id)))
}

fn discard(repository: &Path, target: &Path) -> Result<(), Failure> {
    let _ = git(
        repository,
        &[
            "worktree",
            "remove",
            "--force",
            "--force",
            &target.to_string_lossy(),
        ],
        LOCAL_GIT_TIMEOUT,
    );
    if let Ok(metadata) = std::fs::symlink_metadata(target) {
        let removed = if metadata.is_dir() {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_file(target)
        };
        removed.map_err(|_| {
            Failure::Other("an unfinished worktree folder could not be removed".into())
        })?;
    }
    let _ = git(repository, &["worktree", "prune"], LOCAL_GIT_TIMEOUT);
    Ok(())
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git failed");
    line.chars()
        .filter(|ch| !ch.is_control())
        .take(200)
        .collect()
}

pub fn repository_state(path: &Path) -> RepositoryState {
    let digest = |is_git: bool, head: &Option<String>, branches: &[String]| {
        let mut hash = Sha256::new();
        hash.update([u8::from(is_git)]);
        hash.update(head.as_deref().unwrap_or("").as_bytes());
        for branch in branches {
            hash.update([0]);
            hash.update(branch.as_bytes());
        }
        format!("{:x}", hash.finalize())
    };
    if !is_toplevel(path) {
        return RepositoryState {
            is_git: false,
            head_branch: None,
            branches: Vec::new(),
            digest: digest(false, &None, &[]),
        };
    }
    let head_branch = git_line(path, &["symbolic-ref", "--short", "--quiet", "HEAD"])
        .filter(|name| !name.starts_with(BRANCH_PREFIX));
    let branches = git(
        path,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
        LOCAL_GIT_TIMEOUT,
    )
    .filter(|output| output.ok)
    .map(|output| branch_names(&output.stdout))
    .unwrap_or_default();
    let digest = digest(true, &head_branch, &branches);
    RepositoryState {
        is_git: true,
        head_branch,
        branches,
        digest,
    }
}

fn branch_names(refs: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    refs.lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("refs/heads/")
                .or_else(|| line.strip_prefix("refs/remotes/origin/"))
        })
        .filter(|name| *name != "HEAD" && !name.starts_with(BRANCH_PREFIX))
        .filter(|name| seen.insert((*name).to_owned()))
        .take(MAX_BRANCHES)
        .map(str::to_owned)
        .collect()
}

fn read_records(state: &Path) -> Vec<Record> {
    let Ok(dir) = worktrees_dir(state) else {
        return Vec::new();
    };
    let Ok(entries) = dir.entries() else {
        return Vec::new();
    };
    let mut records: Vec<Record> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.ends_with(".json")
                .then(|| read_record(&dir, &name))
                .flatten()
        })
        .collect();
    records.sort_by_key(|record| (std::cmp::Reverse(record.last_used), record.filesystem_id));
    records
}

fn is_clean(path: &Path) -> Option<bool> {
    let output = git(
        path,
        &[
            "-c",
            "core.fsmonitor=false",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
        LOCAL_GIT_TIMEOUT,
    )?;
    output.ok.then(|| output.stdout.trim().is_empty())
}

pub fn list(state: &Path) -> Vec<Listed> {
    read_records(state)
        .into_iter()
        .map(|record| {
            let path = state
                .join(WORKTREES_DIR)
                .join(record.filesystem_id.to_string());
            let status = if !path.exists() {
                "missing"
            } else {
                match is_clean(&path) {
                    Some(true) => "clean",
                    Some(false) => "dirty",
                    None => "unreadable",
                }
            };
            Listed {
                record,
                path,
                state: status,
            }
        })
        .collect()
}

fn contains_branch(repository: &Path, record: &Record) -> bool {
    let tip = format!("refs/heads/{}", record.branch);
    if !local_branch(repository, &record.branch) {
        return false;
    }
    let pushed = git(
        repository,
        &["for-each-ref", "--contains", &tip, "refs/remotes"],
        LOCAL_GIT_TIMEOUT,
    )
    .is_some_and(|output| output.ok && !output.stdout.trim().is_empty());
    let merged = [
        format!("refs/heads/{}", record.base),
        format!("refs/remotes/origin/{}", record.base),
        "HEAD".to_owned(),
    ]
    .iter()
    .any(|base| {
        git_ok(
            repository,
            &["merge-base", "--is-ancestor", &tip, base.as_str()],
        )
    });
    pushed || merged
}

pub fn prune(state: &Path, now: SystemTime, max_idle: Duration) -> Vec<Pruned> {
    let now = now_secs(now);
    let mut results = Vec::new();
    for record in read_records(state) {
        if now.saturating_sub(record.last_used) < max_idle.as_secs() {
            continue;
        }
        let path = state
            .join(WORKTREES_DIR)
            .join(record.filesystem_id.to_string());
        let outcome = prune_one(state, &record, &path);
        results.push(Pruned {
            filesystem_id: record.filesystem_id,
            branch: record.branch,
            path,
            outcome,
        });
    }
    results
}

fn prune_one(state: &Path, record: &Record, path: &Path) -> &'static str {
    if path.exists() {
        match is_clean(path) {
            Some(true) => {}
            Some(false) => return "kept: uncommitted changes",
            None => return "kept: unreadable",
        }
        let removed = git_ok(
            &record.repository,
            &["worktree", "remove", &path.to_string_lossy()],
        );
        if !removed || path.exists() {
            return "kept: git refused to remove it";
        }
    } else {
        let _ = git(
            &record.repository,
            &["worktree", "prune"],
            LOCAL_GIT_TIMEOUT,
        );
    }
    let deleted = contains_branch(&record.repository, record)
        && git_ok(&record.repository, &["branch", "-D", &record.branch]);
    if let Ok(dir) = worktrees_dir(state) {
        let _ = dir.remove_file(record_name(record.filesystem_id));
    }
    if deleted {
        "removed with branch"
    } else {
        "removed, branch kept"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repository(root: &Path) -> PathBuf {
        let repo = root.join("project");
        std::fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(repo.join("readme.md"), "base").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "--quiet", "-m", "base"]);
        repo.canonicalize().unwrap()
    }

    fn state(root: &Path) -> PathBuf {
        let state = root.join("state");
        private_directory(&state).unwrap();
        state
    }

    fn workspace(base: &str) -> Workspace {
        Workspace {
            filesystem_id: Uuid::new_v4(),
            base_branch: base.into(),
        }
    }

    #[test]
    fn a_worktree_is_created_beside_the_state_and_leaves_the_main_checkout_alone() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        let state = state(temporary.path());
        let workspace = workspace("main");
        let path = ensure(&state, &repo, &workspace).unwrap();
        assert!(path.starts_with(state.canonicalize().unwrap()));
        assert!(path.join("readme.md").is_file());
        std::fs::write(path.join("work.txt"), "edit").unwrap();
        assert!(!repo.join("work.txt").exists());
        let branch = git_line(&path, &["symbolic-ref", "--short", "HEAD"]).unwrap();
        assert_eq!(branch, branch_name(workspace.filesystem_id));
        assert_eq!(ensure(&state, &repo, &workspace).unwrap(), path);
        let listed = list(&state);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, "dirty");
    }

    #[test]
    fn an_interrupted_worktree_add_is_rebuilt_instead_of_served() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        let state = state(temporary.path());
        let workspace = workspace("main");
        let id = workspace.filesystem_id.to_string();
        let target = state.join(WORKTREES_DIR).join(&id);
        let branch = branch_name(workspace.filesystem_id);
        worktrees_dir(&state).unwrap();
        run(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-checkout",
                "-b",
                &branch,
                &target.to_string_lossy(),
                "main",
            ],
        );
        std::fs::write(
            repo.join(".git").join("worktrees").join(&id).join("locked"),
            "initializing",
        )
        .unwrap();
        assert!(is_toplevel(&target));
        assert_eq!(is_clean(&target), Some(false));
        let path = ensure(&state, &repo, &workspace).unwrap();
        assert_eq!(path, target.canonicalize().unwrap());
        assert!(path.join("readme.md").is_file());
        assert_eq!(is_clean(&path), Some(true));
        assert!(
            !repo
                .join(".git")
                .join("worktrees")
                .join(&id)
                .join("locked")
                .exists()
        );
        let listed = list(&state);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].state, "clean");
        std::fs::write(path.join("work.txt"), "edit").unwrap();
        assert_eq!(ensure(&state, &repo, &workspace).unwrap(), path);
        assert!(path.join("work.txt").is_file());
        let later = SystemTime::now() + Duration::from_secs(15 * 24 * 60 * 60);
        let pruned = prune(&state, later, MAX_IDLE);
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].outcome, "kept: uncommitted changes");
    }

    #[tokio::test]
    async fn a_cached_worktree_resolves_while_another_worktree_is_being_created() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        let identity = Arc::new(
            WorkdirIdentity::open(&repo, &state(temporary.path()), Uuid::new_v4()).unwrap(),
        );
        let worktrees = Worktrees::new(&identity.state);
        let workspace = workspace("main");
        let created = worktrees.resolve(&identity, &workspace).await.unwrap();
        let gate = worktrees.gate(&identity.path).await;
        let _creating = gate.lock().await;
        let cached = tokio::time::timeout(
            Duration::from_secs(5),
            worktrees.resolve(&identity, &workspace),
        )
        .await
        .expect("a cached worktree waited for another creation")
        .unwrap();
        assert!(Arc::ptr_eq(&created, &cached));
    }

    #[test]
    fn a_missing_branch_is_reported_with_the_fetch_or_pick_fix() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        let state = state(temporary.path());
        let failure = ensure(&state, &repo, &workspace("release/9")).unwrap_err();
        assert_eq!(failure, Failure::BranchMissing("release/9".into()));
        assert!(
            failure
                .message()
                .contains("Fetch it on the machine, or pick another branch.")
        );
    }

    #[test]
    fn a_folder_that_is_not_a_git_repository_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let plain = temporary.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let state = state(temporary.path());
        assert_eq!(
            ensure(&state, &plain.canonicalize().unwrap(), &workspace("main")).unwrap_err(),
            Failure::NotGit
        );
        assert!(!repository_state(&plain).is_git);
    }

    #[test]
    fn the_repository_summary_lists_recent_branches_without_our_own() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        run(&repo, &["branch", "feature/a"]);
        let state = state(temporary.path());
        ensure(&state, &repo, &workspace("main")).unwrap();
        let summary = repository_state(&repo);
        assert!(summary.is_git);
        assert_eq!(summary.head_branch.as_deref(), Some("main"));
        assert!(summary.branches.contains(&"feature/a".to_owned()));
        assert!(
            summary
                .branches
                .iter()
                .all(|name| !name.starts_with(BRANCH_PREFIX))
        );
        assert_eq!(summary.digest, repository_state(&repo).digest);
        run(&repo, &["branch", "feature/b"]);
        assert_ne!(summary.digest, repository_state(&repo).digest);
    }

    #[test]
    fn prune_removes_only_idle_clean_worktrees_and_keeps_unmerged_branches() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = repository(temporary.path());
        let state = state(temporary.path());
        let clean = workspace("main");
        let dirty = workspace("main");
        let clean_path = ensure(&state, &repo, &clean).unwrap();
        let dirty_path = ensure(&state, &repo, &dirty).unwrap();
        std::fs::write(dirty_path.join("wip.txt"), "wip").unwrap();
        std::fs::write(clean_path.join("note.txt"), "n").unwrap();
        run(&clean_path, &["add", "."]);
        run(&clean_path, &["commit", "--quiet", "-m", "note"]);
        let later = SystemTime::now() + Duration::from_secs(15 * 24 * 60 * 60);
        assert!(prune(&state, SystemTime::now(), MAX_IDLE).is_empty());
        let results = prune(&state, later, MAX_IDLE);
        assert_eq!(results.len(), 2);
        assert!(!clean_path.exists());
        assert!(dirty_path.exists());
        let by_id = |id| results.iter().find(|r| r.filesystem_id == id).unwrap();
        assert_eq!(by_id(clean.filesystem_id).outcome, "removed, branch kept");
        assert_eq!(
            by_id(dirty.filesystem_id).outcome,
            "kept: uncommitted changes"
        );
        assert!(local_branch(&repo, &branch_name(clean.filesystem_id)));
        assert_eq!(list(&state).len(), 1);
    }
}
