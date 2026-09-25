use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt, PermissionsExt};
use cloudthinker_client::auth::worker_store::{private_directory, read_private, write_private};
use cloudthinker_client::{CtError, CtResult, worker_types as api};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    Received,
    Started,
    Terminal,
    Acknowledged,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRecord {
    pub operation_id: Uuid,
    pub assignment_id: Uuid,
    pub session_id: Uuid,
    pub sequence: i64,
    pub fence: i64,
    pub digest: String,
    pub effect: api::OperationEffect,
    pub state: JournalState,
    pub result_digest: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Operation(OperationRecord),
    Cursor { sequence: u64 },
}

pub struct ReceiptJournal {
    dir: Dir,
    file: cap_std::fs::File,
    records: BTreeMap<Uuid, OperationRecord>,
    cursor: u64,
    appended: usize,
}

#[derive(Clone)]
pub struct Journal(std::sync::Arc<std::sync::Mutex<ReceiptJournal>>);

impl Journal {
    pub async fn open(path: std::path::PathBuf) -> CtResult<Self> {
        tokio::task::spawn_blocking(move || {
            ReceiptJournal::open(&path)
                .map(|journal| Self(std::sync::Arc::new(std::sync::Mutex::new(journal))))
        })
        .await
        .map_err(|_| journal_error())?
    }

    pub async fn apply<R: Send + 'static>(
        &self,
        action: impl FnOnce(&mut ReceiptJournal) -> CtResult<R> + Send + 'static,
    ) -> CtResult<R> {
        let journal = self.0.clone();
        tokio::task::spawn_blocking(move || {
            action(&mut *journal.lock().map_err(|_| journal_error())?)
        })
        .await
        .map_err(|_| journal_error())?
    }

    pub async fn retire(&self, path: std::path::PathBuf) -> CtResult<()> {
        let cleared = self
            .apply(|journal| {
                if journal
                    .records
                    .values()
                    .any(|record| record.state != JournalState::Acknowledged)
                {
                    return Ok(false);
                }
                journal.compact()?;
                Ok(true)
            })
            .await?;
        if !cleared {
            return Ok(());
        }
        tokio::task::spawn_blocking(move || {
            let dir = private_directory(&path)?;
            if dir.entries().map_err(|_| journal_error())?.take(2).count() != 1 {
                return Ok(());
            }
            dir.remove_file("journal.jsonl")
                .map_err(|_| journal_error())?;
            dir.open(".")
                .map_err(|_| journal_error())?
                .sync_all()
                .map_err(|_| journal_error())?;
            let parent = private_directory(path.parent().ok_or_else(journal_error)?)?;
            parent
                .remove_dir(path.file_name().ok_or_else(journal_error)?)
                .map_err(|_| journal_error())?;
            parent
                .open(".")
                .map_err(|_| journal_error())?
                .sync_all()
                .map_err(|_| journal_error())
        })
        .await
        .map_err(|_| journal_error())?
    }
}

impl ReceiptJournal {
    pub fn open(path: &Path) -> CtResult<Self> {
        let dir = private_directory(path)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        let file = dir
            .open_with("journal.jsonl", &options)
            .map_err(|_| journal_error())?;
        let metadata = file.metadata().map_err(|_| journal_error())?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(journal_error());
        }
        let mut journal = Self {
            dir,
            file,
            records: BTreeMap::new(),
            cursor: 0,
            appended: 0,
        };
        let mut reader = BufReader::new(journal.file.try_clone().map_err(|_| journal_error())?);
        let mut line = Vec::new();
        let mut valid_length = 0;
        loop {
            line.clear();
            let count = (&mut reader)
                .take(16385)
                .read_until(b'\n', &mut line)
                .map_err(|_| journal_error())?;
            if count == 0 {
                break;
            }
            if count > 16384 {
                return Err(journal_error());
            }
            if line.last() != Some(&b'\n') {
                journal
                    .file
                    .set_len(valid_length)
                    .map_err(|_| journal_error())?;
                journal.file.sync_all().map_err(|_| journal_error())?;
                break;
            }
            let record: Record = serde_json::from_slice(&line).map_err(|_| journal_error())?;
            match record {
                Record::Operation(operation) => {
                    if operation.state == JournalState::Acknowledged {
                        journal.remove_result(operation.operation_id)?;
                        journal.records.remove(&operation.operation_id);
                    } else {
                        journal.records.insert(operation.operation_id, operation);
                    }
                    if journal.records.len() > 4096 {
                        return Err(journal_error());
                    }
                }
                Record::Cursor { sequence } => {
                    journal.cursor = sequence;
                }
            }
            valid_length += count as u64;
        }
        journal.compact()?;
        Ok(journal)
    }

    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn records(&self) -> Vec<OperationRecord> {
        self.records.values().cloned().collect()
    }

    pub fn received(&mut self, envelope: &api::OperationEnvelope) -> CtResult<JournalState> {
        if let Some(record) = self.records.get(&envelope.operation_id) {
            if record.digest != envelope.request_digest
                || record.sequence != envelope.operation_sequence
                || record.session_id != envelope.session_id
                || record.assignment_id != envelope.assignment_id
            {
                return Err(journal_error());
            }
            if record.fence != envelope.fence_token && record.state != JournalState::Acknowledged {
                let replayable = matches!(
                    (&record.effect, &envelope.effect),
                    (api::OperationEffect::Read, api::OperationEffect::Read)
                        | (
                            api::OperationEffect::IdempotentMutation,
                            api::OperationEffect::IdempotentMutation,
                        )
                );
                if !replayable {
                    return Err(journal_error());
                }
                let redelivered = OperationRecord {
                    fence: envelope.fence_token,
                    state: JournalState::Received,
                    result_digest: None,
                    ..record.clone()
                };
                self.append(&Record::Operation(redelivered.clone()))?;
                self.records.insert(envelope.operation_id, redelivered);
                return Ok(JournalState::Received);
            }
            return Ok(record.state.clone());
        }
        if self.records.len() >= 4096 {
            self.compact()?;
        }
        if self.records.len() >= 4096 {
            return Err(journal_error());
        }
        let record = OperationRecord {
            operation_id: envelope.operation_id,
            assignment_id: envelope.assignment_id,
            session_id: envelope.session_id,
            sequence: envelope.operation_sequence,
            fence: envelope.fence_token,
            digest: envelope.request_digest.clone(),
            effect: envelope.effect,
            state: JournalState::Received,
            result_digest: None,
        };
        self.append(&Record::Operation(record.clone()))?;
        self.records.insert(envelope.operation_id, record);
        Ok(JournalState::Received)
    }

    pub fn advance_cursor(&mut self, sequence: u64) -> CtResult<()> {
        if sequence == self.cursor {
            return Ok(());
        }
        if sequence < self.cursor {
            return Err(journal_error());
        }
        self.append(&Record::Cursor { sequence })?;
        self.cursor = sequence;
        Ok(())
    }

    pub fn started(&mut self, operation: Uuid) -> CtResult<()> {
        self.transition(operation, JournalState::Started, None)
    }

    pub fn terminal(
        &mut self,
        operation: Uuid,
        result: &api::WorkerOperationResult,
    ) -> CtResult<()> {
        let bytes = serde_json::to_vec(result).map_err(|_| journal_error())?;
        if bytes.len() > 2_800_000 {
            return Err(journal_error());
        }
        let digest = format!("{:x}", Sha256::digest(&bytes));
        write_private(&self.dir, &format!("{operation}.result"), &bytes)?;
        self.transition(operation, JournalState::Terminal, Some(digest))
    }

    pub fn result(&self, operation: Uuid) -> CtResult<api::WorkerOperationResult> {
        let record = self.records.get(&operation).ok_or_else(journal_error)?;
        let bytes = read_private(&self.dir, &format!("{operation}.result"), 2_800_000)?;
        if record.result_digest.as_deref() != Some(&format!("{:x}", Sha256::digest(&bytes))) {
            return Err(journal_error());
        }
        serde_json::from_slice(&bytes).map_err(|_| journal_error())
    }

    pub fn acknowledge(&mut self, operation: Uuid) -> CtResult<()> {
        self.transition(operation, JournalState::Acknowledged, None)?;
        match self.dir.remove_file(format!("{operation}.result")) {
            Ok(()) => self
                .dir
                .open(".")
                .map_err(|_| journal_error())?
                .sync_all()
                .map_err(|_| journal_error()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(journal_error()),
        }
    }

    fn transition(
        &mut self,
        operation: Uuid,
        state: JournalState,
        result_digest: Option<String>,
    ) -> CtResult<()> {
        let previous = self.records.get(&operation).ok_or_else(journal_error)?;
        let allowed = matches!(
            (&previous.state, &state),
            (JournalState::Received, JournalState::Started)
                | (JournalState::Started, JournalState::Terminal)
                | (JournalState::Terminal, JournalState::Acknowledged)
                | (
                    JournalState::Received | JournalState::Started,
                    JournalState::Acknowledged
                )
        );
        if !allowed {
            return Err(journal_error());
        }
        let record = OperationRecord {
            state,
            result_digest: result_digest.or_else(|| previous.result_digest.clone()),
            ..previous.clone()
        };
        self.append(&Record::Operation(record.clone()))?;
        self.records.insert(operation, record);
        if self.appended >= 1024 {
            self.compact()?;
        }
        Ok(())
    }

    fn remove_result(&self, operation: Uuid) -> CtResult<()> {
        match self.dir.remove_file(format!("{operation}.result")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(journal_error()),
        }
    }

    fn compact(&mut self) -> CtResult<()> {
        for record in self
            .records
            .values()
            .filter(|r| r.state == JournalState::Acknowledged)
        {
            self.remove_result(record.operation_id)?;
        }
        let mut bytes = Vec::new();
        for record in self
            .records
            .values()
            .filter(|r| r.state != JournalState::Acknowledged)
        {
            serde_json::to_writer(&mut bytes, &Record::Operation(record.clone()))
                .map_err(|_| journal_error())?;
            bytes.push(b'\n');
        }
        serde_json::to_writer(
            &mut bytes,
            &Record::Cursor {
                sequence: self.cursor,
            },
        )
        .map_err(|_| journal_error())?;
        bytes.push(b'\n');
        write_private(&self.dir, "journal.jsonl", &bytes)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .append(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        self.file = self
            .dir
            .open_with("journal.jsonl", &options)
            .map_err(|_| journal_error())?;
        self.records
            .retain(|_, r| r.state != JournalState::Acknowledged);
        self.appended = 0;
        Ok(())
    }

    fn append(&mut self, record: &Record) -> CtResult<()> {
        self.appended += 1;
        let mut bytes = serde_json::to_vec(record).map_err(|_| journal_error())?;
        bytes.push(b'\n');
        self.file.write_all(&bytes).map_err(|_| journal_error())?;
        self.file.sync_data().map_err(|_| journal_error())
    }
}

fn journal_error() -> CtError {
    CtError::Store(
        "worker receipt journal is unavailable or inconsistent; execution stopped".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> api::OperationEnvelope {
        serde_json::from_value(serde_json::json!({ "target_id": Uuid::new_v4(), "session_id": Uuid::new_v4(), "assignment_id": Uuid::new_v4(), "operation_id": Uuid::new_v4(), "operation_sequence": 1, "fence_token": 1, "lease_token": "private-lease", "nonce": "private-nonce", "deadline_at": "2030-01-01T00:00:00Z", "effect": "mutation", "payload": { "kind": "script", "script": "private-script" }, "request_digest": "a".repeat(64) })).unwrap()
    }

    #[test]
    fn open_makes_the_journal_file_durable_before_any_append() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let env = envelope();
        let mut journal = ReceiptJournal::open(&path).unwrap();
        assert!(path.join("journal.jsonl").is_file());
        journal.received(&env).unwrap();
        journal.started(env.operation_id).unwrap();
        drop(journal);
        let recovered = ReceiptJournal::open(&path).unwrap();
        assert_eq!(recovered.records().len(), 1);
        assert!(recovered.records()[0].state == JournalState::Started);
    }

    #[test]
    fn compaction_preserves_uncertain_operations_and_bounds_completed_history() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        let uncertain = envelope();
        journal.received(&uncertain).unwrap();
        journal.started(uncertain.operation_id).unwrap();
        for sequence in 2..1800 {
            let mut next = envelope();
            next.operation_sequence = sequence;
            journal.received(&next).unwrap();
            journal.acknowledge(next.operation_id).unwrap();
            journal.advance_cursor(sequence as u64).unwrap();
        }
        drop(journal);
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        assert_eq!(journal.cursor(), 1799);
        assert!(journal.received(&uncertain).unwrap() == JournalState::Started);
        assert_eq!(journal.records().len(), 1);
        assert!(
            std::fs::metadata(root.path().join("journal/journal.jsonl"))
                .unwrap()
                .len()
                < 2048
        );
    }

    #[test]
    fn ca_wo_30_started_survives_restart_and_32_no_payload_in_journal() {
        let root = tempfile::tempdir().unwrap();
        let env = envelope();
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        journal.received(&env).unwrap();
        journal.advance_cursor(1).unwrap();
        journal.started(env.operation_id).unwrap();
        drop(journal);
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        assert!(journal.received(&env).unwrap() == JournalState::Started);
        assert_eq!(journal.cursor(), 1);
        assert!(journal.started(env.operation_id).is_err());
        let text = std::fs::read_to_string(root.path().join("journal/journal.jsonl")).unwrap();
        assert!(!text.contains("private-"));
        assert!(!text.contains("script"));
    }

    #[test]
    fn ca_wo_30_torn_records_recover_only_the_last_durable_state() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("journal");
        let env = envelope();
        let mut journal = ReceiptJournal::open(&path).unwrap();
        let snapshot = || std::fs::read(path.join("journal.jsonl")).unwrap();
        let mut stages = vec![(snapshot(), None, 0)];
        journal.received(&env).unwrap();
        stages.push((snapshot(), Some(JournalState::Received), 0));
        journal.advance_cursor(1).unwrap();
        stages.push((snapshot(), Some(JournalState::Received), 1));
        journal.started(env.operation_id).unwrap();
        stages.push((snapshot(), Some(JournalState::Started), 1));
        let result: api::WorkerOperationResult = serde_json::from_value(
            serde_json::json!({"state": "succeeded", "status_code": 200, "content_base64": "e30="}),
        )
        .unwrap();
        journal.terminal(env.operation_id, &result).unwrap();
        stages.push((snapshot(), Some(JournalState::Terminal), 1));
        let result_name = format!("{}.result", env.operation_id);
        let result_bytes = std::fs::read(path.join(&result_name)).unwrap();
        drop(journal);

        for pair in stages.windows(2) {
            let before = &pair[0];
            let after = &pair[1];
            let record = &after.0[before.0.len()..];
            for retained in [0, 1, record.len() / 2, record.len() - 1, record.len()] {
                let root = tempfile::tempdir().unwrap();
                let recovered_path = root.path().join("journal");
                let dir = private_directory(&recovered_path).unwrap();
                let mut bytes = before.0.clone();
                bytes.extend_from_slice(&record[..retained]);
                write_private(&dir, "journal.jsonl", &bytes).unwrap();
                write_private(&dir, &result_name, &result_bytes).unwrap();
                let mut recovered = ReceiptJournal::open(&recovered_path).unwrap();
                let expected = if retained == record.len() {
                    after
                } else {
                    before
                };
                assert_eq!(recovered.cursor(), expected.2);
                let records = recovered.records();
                match &expected.1 {
                    Some(state) => {
                        assert_eq!(records.len(), 1);
                        assert_eq!(records[0].operation_id, env.operation_id);
                        assert_eq!(records[0].assignment_id, env.assignment_id);
                        assert_eq!(records[0].session_id, env.session_id);
                        assert_eq!(records[0].sequence, env.operation_sequence);
                        assert_eq!(records[0].fence, env.fence_token);
                        assert_eq!(records[0].digest, env.request_digest);
                        assert_eq!(records[0].effect, env.effect);
                        assert!(records[0].state == *state);
                        assert!(recovered.received(&env).unwrap() == *state);
                        if *state == JournalState::Terminal {
                            assert_eq!(
                                records[0].result_digest,
                                Some(format!("{:x}", Sha256::digest(&result_bytes)))
                            );
                            assert_eq!(
                                recovered.result(env.operation_id).unwrap().content_base64,
                                result.content_base64
                            );
                        } else {
                            assert!(records[0].result_digest.is_none());
                            assert!(recovered.result(env.operation_id).is_err());
                        }
                        if *state != JournalState::Received {
                            assert!(recovered.started(env.operation_id).is_err());
                        }
                    }
                    None => assert!(records.is_empty()),
                }
                drop(recovered);
                let reopened = ReceiptJournal::open(&recovered_path).unwrap();
                assert_eq!(reopened.cursor(), expected.2);
                assert_eq!(reopened.records().len(), usize::from(expected.1.is_some()));
                assert_eq!(
                    std::fs::read(recovered_path.join(&result_name)).unwrap(),
                    result_bytes
                );
            }
        }
    }

    #[test]
    fn ca_wo_32_complete_corrupt_record_stops_recovery() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let env = envelope();
        let mut journal = ReceiptJournal::open(&path).unwrap();
        journal.received(&env).unwrap();
        journal.started(env.operation_id).unwrap();
        drop(journal);
        let file = path.join("journal.jsonl");
        let prefix = std::fs::read(&file).unwrap();
        for corrupt in [
            "{invalid-record}\n",
            "{\"kind\":\"unknown\"}\n",
            "{\"kind\":\"cursor\",\"sequence\":1,\"extra\":true}\n",
        ] {
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(corrupt.as_bytes());
            std::fs::write(&file, &bytes).unwrap();
            assert!(ReceiptJournal::open(&path).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), bytes);
        }
    }

    #[test]
    fn ca_wo_32_acknowledged_record_recovery_removes_result_spool() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal");
        let env = envelope();
        let mut journal = ReceiptJournal::open(&path).unwrap();
        journal.received(&env).unwrap();
        journal.started(env.operation_id).unwrap();
        let result: api::WorkerOperationResult = serde_json::from_value(
            serde_json::json!({"state": "succeeded", "status_code": 200, "content_base64": "e30="}),
        )
        .unwrap();
        journal.terminal(env.operation_id, &result).unwrap();
        let result_path = path.join(format!("{}.result", env.operation_id));
        assert!(result_path.is_file());

        journal
            .transition(env.operation_id, JournalState::Acknowledged, None)
            .unwrap();
        assert!(result_path.is_file());
        drop(journal);

        let recovered = ReceiptJournal::open(&path).unwrap();
        assert!(recovered.records().is_empty());
        assert!(!result_path.exists());
        drop(recovered);

        let reopened = ReceiptJournal::open(&path).unwrap();
        assert!(reopened.records().is_empty());
        assert!(!result_path.exists());
    }

    #[test]
    fn ca_wo_38_idempotent_mutation_redelivers_after_fence_but_mutation_does_not() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        let mut idempotent = envelope();
        idempotent.effect = api::OperationEffect::IdempotentMutation;
        journal.received(&idempotent).unwrap();
        journal.started(idempotent.operation_id).unwrap();
        let mut replay = idempotent.clone();
        replay.fence_token = 2;
        assert!(journal.received(&replay).unwrap() == JournalState::Received);
        assert_eq!(journal.records()[0].fence, 2);

        let ordinary = envelope();
        journal.received(&ordinary).unwrap();
        journal.started(ordinary.operation_id).unwrap();
        let mut ordinary_replay = ordinary.clone();
        ordinary_replay.fence_token = 2;
        assert!(journal.received(&ordinary_replay).is_err());
    }

    #[test]
    fn ca_wo_12_terminal_spool_replays_and_missing_spool_never_reexecutes() {
        let root = tempfile::tempdir().unwrap();
        let env = envelope();
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        journal.received(&env).unwrap();
        journal.started(env.operation_id).unwrap();
        let result: api::WorkerOperationResult = serde_json::from_value(
            serde_json::json!({"state": "succeeded", "status_code": 200, "content_base64": "e30="}),
        )
        .unwrap();
        journal.terminal(env.operation_id, &result).unwrap();
        drop(journal);
        let mut journal = ReceiptJournal::open(&root.path().join("journal")).unwrap();
        assert_eq!(
            journal
                .result(env.operation_id)
                .unwrap()
                .content_base64
                .to_string(),
            "e30="
        );
        std::fs::remove_file(
            root.path()
                .join("journal")
                .join(format!("{}.result", env.operation_id)),
        )
        .unwrap();
        assert!(journal.result(env.operation_id).is_err());
        assert!(journal.received(&env).unwrap() == JournalState::Terminal);
    }
}
