//! End-to-end binary tests driving `cloudthinker` against a canned HTTP server.
//!
//! `CLOUDTHINKER_TOKEN` supplies an access-only credential so no keyring or
//! login is involved, and the mock server returns terminal runs immediately so
//! the watch loop finishes in one poll.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// This integration target links the whole crate's dependency set but only needs
// a few; silence `unused_crate_dependencies` for the bin-only deps.
use clap as _;
use cloudthinker_client as _;
use open as _;
use owo_colors as _;
use rand as _;
use serde as _;
use supports_color as _;
use tokio as _;
use uuid as _;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use assert_cmd::Command;

const RUN_ID: &str = "11111111-1111-4111-8111-111111111111";
const CONV_ID: &str = "22222222-2222-4222-8222-222222222222";

/// A canned HTTP/1.1 server: 202 on submit, a fixed body on status GET.
struct MockApi {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockApi {
    fn start(get_status_body: String) -> Self {
        Self::start_with_status("200 OK", get_status_body)
    }

    /// Like `start`, but the non-POST (GET) response uses `get_status_line`
    /// instead of the hardcoded "200 OK" — lets a test drive a 404, e.g.
    /// CA-RV-SP4's unknown-coordinates review lookup.
    fn start_with_status(get_status_line: &str, get_status_body: String) -> Self {
        let get_status_line = get_status_line.to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();

        let submit_body = format!(
            r#"{{"run_id":"{RUN_ID}","conversation_id":"{CONV_ID}","status":"running","web_url":"https://app.example.com/c/{CONV_ID}"}}"#
        );

        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Drain the whole request (headers + body) before
                        // responding, so we never RST a client still sending its
                        // POST body. The accepted socket can inherit the
                        // listener's non-blocking flag — force blocking + a read
                        // timeout so the first read waits for data to arrive.
                        socket.set_nonblocking(false).ok();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .ok();
                        let mut data = Vec::new();
                        let mut buf = [0u8; 4096];
                        // Read until the client pauses (read timeout) or EOF —
                        // the client keeps its write side open awaiting the
                        // response, so the timeout is the drain signal.
                        loop {
                            match socket.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => data.extend_from_slice(&buf[..n]),
                                Err(_) => break,
                            }
                        }
                        let request = String::from_utf8_lossy(&data);
                        let first_line = request.lines().next().unwrap_or_default();
                        let (status, body) = if first_line.starts_with("POST") {
                            ("202 Accepted", submit_body.clone())
                        } else {
                            (get_status_line.as_str(), get_status_body.clone())
                        };
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes());
                        let _ = socket.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn status_body(status: &str, answer: &str, failure_kind: &str) -> String {
    let answer_json = if answer.is_empty() {
        "null".to_string()
    } else {
        format!("\"{answer}\"")
    };
    let failure_json = if failure_kind.is_empty() {
        "null".to_string()
    } else {
        format!("\"{failure_kind}\"")
    };
    format!(
        r#"{{"run_id":"{RUN_ID}","conversation_id":"{CONV_ID}","status":"{status}","answer":{answer_json},"message":null,"failure_kind":{failure_json},"web_url":"https://app.example.com/c/{CONV_ID}","created_at":"2026-07-20T00:00:00Z","start_time":null,"end_time":null}}"#
    )
}

fn cli(base_url: &str) -> Command {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token")
        .env("CLOUDTHINKER_URL", base_url)
        .env_remove("NO_COLOR");
    cmd
}

const MR_URL: &str = "https://gitlab.example.com/group/my-repo/-/merge_requests/42";

fn review_body(review_status: &str, verdict: &str, findings_count: i64) -> String {
    format!(
        r#"{{"id":"33333333-3333-4333-8333-333333333333","created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-20T00:00:00Z","mr_iid":42,"mr_state":"open","provider":"gitlab","repository_name":"my-repo","repository_path":"group/my-repo","review_status":"{review_status}","severity_counts":{{"critical":1,"high":0,"medium":0,"low":0}},"title":"Fix the bug","verdict":"{verdict}","findings_count":{findings_count},"url":"https://gitlab.example.com/group/my-repo/-/merge_requests/42","findings":[{{"id":"44444444-4444-4444-8444-444444444444","finding_index":0,"issue_title":"possible SQL injection","issue_description":"unsanitized input reaches the query","severity":"critical","severity_emoji":"🔴","provider":"gitlab","file_path":"app/db.py","line_number":10,"category":"security","resolved":false,"resolved_at":null,"resolved_by":null,"external_comment_id":null,"external_note_id":null,"side":null,"specialist":null,"suggested_fix":null,"comment_posted_at":null,"created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-20T00:00:00Z"}}]}}"#
    )
}

// CA-RV-1 / status happy path: `status <URL>` prints the verdict + status +
// findings count and exits 0.
#[test]
fn ca_rv_1_status_prints_summary_and_exits_0() {
    let api = MockApi::start(review_body("review_complete", "changes_requested", 1));
    cli(&api.base_url)
        .args(["review", "status", MR_URL])
        .assert()
        .success()
        .stdout(predicates::str::contains("review complete"))
        .stdout(predicates::str::contains("changes requested"));
}

// CA-RV-2: `findings <URL>` lists findings and exits 0 on a successful read
// regardless of the review's own verdict (CA-RV-SP5).
#[test]
fn ca_rv_2_findings_lists_findings_and_exits_0() {
    let api = MockApi::start(review_body("in_review", "in_review", 1));
    cli(&api.base_url)
        .args(["review", "findings", MR_URL])
        .assert()
        .success()
        .stdout(predicates::str::contains("possible SQL injection"));
}

// CA-RV-3 (success half): `watch <URL>` polls to a terminal review and exits 0.
#[test]
fn ca_rv_3_watch_terminal_review_complete_exits_0() {
    let api = MockApi::start(review_body("review_complete", "approved", 0));
    cli(&api.base_url)
        .args(["review", "watch", MR_URL])
        .assert()
        .success()
        .stdout(predicates::str::contains("approved"));
}

// CA-RV-3 (failed half): a FAILED review_status exits 1, not 0.
#[test]
fn ca_rv_3_watch_failed_review_status_exits_1() {
    let api = MockApi::start(review_body("failed", "failed", 0));
    cli(&api.base_url)
        .args(["review", "watch", MR_URL])
        .assert()
        .code(1);
}

// CA-RV-SP1: an unparseable URL exits 2 with no request attempted — proven by
// pointing at a closed port; a real request there would surface as a
// transport failure (exit 1), not usage (exit 2).
#[test]
fn ca_rv_sp1_unparseable_url_is_usage_before_any_request() {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token")
        .env("CLOUDTHINKER_URL", "http://127.0.0.1:1")
        .args(["review", "status", "https://example.com/not-a-review-url"])
        .assert()
        .code(2);
}

// CA-RV-SP4: unknown coordinates (404) print the review-specific message and
// exit 1.
#[test]
fn ca_rv_sp4_unknown_coordinates_prints_message_and_exits_1() {
    let api = MockApi::start_with_status(
        "404 Not Found",
        serde_json::json!({"detail": "Merge request not found"}).to_string(),
    );
    cli(&api.base_url)
        .args(["review", "status", MR_URL])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("no code review found"));
}

// CA-RV-SP6: `watch`'s client-side timeout prints a resume hint and exits 4;
// a zero timeout elapses before the first poll, so no live review is needed.
#[test]
fn ca_rv_sp6_watch_timeout_prints_resume_hint_and_exits_4() {
    let api = MockApi::start(review_body("in_review", "in_review", 0));
    cli(&api.base_url)
        .args(["review", "watch", MR_URL, "--timeout", "0"])
        .assert()
        .code(4)
        .stderr(predicates::str::contains("review status"));
}

// CA-CLI-10: on success, stdout carries ONLY the answer text (pipeable); the
// "submitted" progress line goes to stderr.
#[test]
fn ca_cli_10_stdout_is_answer_only() {
    let api = MockApi::start(status_body("succeeded", "the final answer", ""));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .success()
        .stdout("the final answer\n")
        .stderr(predicates::str::contains("Submitted"));
}

// CA-CLI-10 (--json envelope): the JSON envelope carries the API field names.
#[test]
fn json_envelope_has_api_field_names() {
    let api = MockApi::start(status_body("succeeded", "hi there", ""));
    let output = cli(&api.base_url)
        .args(["chat", "-p", "hello", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["run_id"], RUN_ID);
    assert_eq!(value["conversation_id"], CONV_ID);
    assert_eq!(value["status"], "succeeded");
    assert_eq!(value["answer"], "hi there");
    assert_eq!(
        value["web_url"],
        format!("https://app.example.com/c/{CONV_ID}")
    );
}

// CA-CLI-11: a FAILED run exits 1 with the reason on stderr.
#[test]
fn ca_cli_11_failed_run_exits_1() {
    let api = MockApi::start(status_body("failed", "", "provider_error"));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("provider_error"));
}

// CA-CLI-12: REQUIRED_APPROVAL exits 5 with the approval URL on stderr.
#[test]
fn ca_cli_12_required_approval_exits_5() {
    let api = MockApi::start(status_body("required_approval", "", ""));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .code(5)
        .stderr(predicates::str::contains("Approve"));
}

// CA-CLI-16: `chat status` of a terminal run renders it and exits 0.
#[test]
fn ca_cli_16_status_of_terminal_run_exits_0() {
    let api = MockApi::start(status_body("succeeded", "cached answer", ""));
    cli(&api.base_url)
        .args(["chat", "status", RUN_ID])
        .assert()
        .success()
        .stdout(predicates::str::contains("succeeded"))
        .stdout(predicates::str::contains("cached answer"));
}

// Usage: `chat` with neither a prompt nor `status` exits 2.
#[test]
fn chat_without_prompt_is_usage_error() {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token")
        .arg("chat")
        .assert()
        .code(2);
}
