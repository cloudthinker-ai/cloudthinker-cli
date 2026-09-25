//! End-to-end binary tests driving `cloudthinker` against a canned HTTP server.
//!
//! `CLOUDTHINKER_TOKEN` supplies an access-only credential so no keyring or
//! login is involved, and the mock server returns terminal runs immediately so
//! the watch loop finishes in one poll.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// This integration target links the whole crate's dependency set but only needs
// a few; silence `unused_crate_dependencies` for the bin-only deps.
use base64 as _;
use cap_std as _;
use chrono as _;
use flate2 as _;
use fs2 as _;
use globset as _;
use regex as _;
#[cfg(unix)]
use rustix as _;
use sha2 as _;
use tar as _;
use tempfile as _;
use tokio_util as _;

use axoupdater as _;
use clap as _;
use cloudthinker_client as _;
use indicatif as _;
use open as _;
use owo_colors as _;
use rand as _;
use serde as _;
use supports_color as _;
use tokio as _;

use std::collections::VecDeque;

use predicates::prelude::PredicateBooleanExt;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use assert_cmd::Command;

const RUN_ID: &str = "11111111-1111-4111-8111-111111111111";
const CONV_ID: &str = "22222222-2222-4222-8222-222222222222";

fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    socket.set_nonblocking(false).ok();
    socket.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let mut expected_len = None;
    loop {
        if expected_len.is_none() {
            expected_len = data
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|end| {
                    let headers = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
                    let body_len = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    end + 4 + body_len
                });
        }
        if expected_len.is_some_and(|len| data.len() >= len) {
            return data;
        }
        match socket.read(&mut buf) {
            Ok(0) | Err(_) => return data,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
}

#[test]
fn ca_ad_11_unknown_command_keeps_clap_suggestions_and_usage_exit() {
    let output = Command::cargo_bin("cloudthinker")
        .unwrap()
        .arg("cht")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("chat"));
}

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
                        let data = read_request(&mut socket);
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

struct RecordingApi {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RecordingApi {
    fn start(responses: Vec<(&str, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_thread = requests.clone();
        let mut responses: VecDeque<(String, String)> = responses
            .into_iter()
            .map(|(status, body)| (status.to_string(), body))
            .collect();

        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let data = read_request(&mut socket);
                        let request = String::from_utf8_lossy(&data).into_owned();
                        requests_thread.lock().unwrap().push(request);
                        let (status, body) = responses
                            .pop_front()
                            .unwrap_or_else(|| ("500 Internal Server Error".into(), "{}".into()));
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes());
                        let _ = socket.flush();
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for RecordingApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn submitted_body() -> String {
    format!(
        r#"{{"run_id":"{RUN_ID}","conversation_id":"{CONV_ID}","status":"pending","web_url":"https://app.example.com/c/{CONV_ID}"}}"#
    )
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
        .env_remove("NO_COLOR")
        .env_remove("CLOUDTHINKER_WORKSPACE")
        .env_remove("CLOUDTHINKER_AGENT_BIN");
    cmd
}

const WHOAMI_BODY: &str = r#"{"user_id":"22222222-2222-4222-8222-222222222222","user_email":"duc@example.com","workspace_id":"11111111-1111-4111-8111-111111111111","workspace_name":"Production","organization_id":null}"#;

#[test]
fn whoami_prints_one_live_identity_line() {
    let api = MockApi::start(WHOAMI_BODY.into());

    cli(&api.base_url)
        .arg("whoami")
        .assert()
        .success()
        .stdout(predicates::str::is_match(
            r"^host=http://127\.0\.0\.1:[0-9]+ email=duc@example\.com workspace=Production \(11111111-1111-4111-8111-111111111111\)\n$",
        ).unwrap());
}

#[test]
fn whoami_json_prints_the_desktop_identity_contract() {
    let api = MockApi::start(WHOAMI_BODY.into());

    let output = cli(&api.base_url)
        .args(["whoami", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "host": api.base_url,
            "user_id": "22222222-2222-4222-8222-222222222222",
            "workspace_id": "11111111-1111-4111-8111-111111111111",
        })
    );
}

#[test]
fn workspace_with_environment_token_is_usage_error_without_stdout() {
    let api = MockApi::start("{}".into());

    cli(&api.base_url)
        .args(["--workspace", "Production", "whoami"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains(
            "--workspace cannot be used with CLOUDTHINKER_TOKEN",
        ));
}

#[test]
fn whoami_with_revoked_credential_is_auth_error_without_stdout() {
    let api = MockApi::start_with_status(
        "401 Unauthorized",
        r#"{"error":{"code":"unauthorized","message":"expired","retryable":false},"detail":"expired"}"#.into(),
    );

    cli(&api.base_url)
        .arg("whoami")
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicates::str::contains("not authenticated"));
}

// `auth token` is consumed as `apiKey: "!cloudthinker auth token"`, so stdout
// carries the token and one newline and nothing else, and stderr stays empty.
// A closed port proves a live credential needs no request to print.
#[test]
fn auth_token_prints_only_the_token_on_stdout() {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token")
        .env("CLOUDTHINKER_URL", "http://127.0.0.1:1")
        .env_remove("NO_COLOR")
        .args(["auth", "token"])
        .assert()
        .success()
        .stdout("test-access-token\n")
        .stderr("");
}

const MR_URL: &str = "https://gitlab.example.com/group/my-repo/-/merge_requests/42";

fn review_body(review_status: &str, verdict: &str, findings_count: i64) -> String {
    format!(
        r#"{{"id":"33333333-3333-4333-8333-333333333333","created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-20T00:00:00Z","mr_iid":42,"mr_state":"open","provider":"gitlab","repository_name":"my-repo","repository_path":"group/my-repo","review_status":"{review_status}","severity_counts":{{"critical":1,"high":0,"medium":0,"low":0}},"title":"Fix the bug","verdict":"{verdict}","findings_count":{findings_count},"url":"https://gitlab.example.com/group/my-repo/-/merge_requests/42","findings":[{{"id":"44444444-4444-4444-8444-444444444444","finding_index":0,"issue_title":"possible SQL injection","issue_description":"unsanitized input reaches the query","severity":"critical","severity_emoji":"🔴","provider":"gitlab","file_path":"app/db.py","line_number":10,"category":"security","resolved":false,"acknowledged":false,"withdrawn":false,"resolved_at":null,"resolved_by":null,"external_comment_id":null,"external_note_id":null,"side":null,"specialist":null,"suggested_fix":null,"comment_posted_at":null,"created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-20T00:00:00Z"}}]}}"#
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

// CA-RV-SP4: legacy detail-only unknown coordinates keep their 404 status,
// print the review-specific message, and exit 1.
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
// "submitted" progress line goes to stderr and names the cloud workspace, so a
// user in a repo directory is never told the run saw their local files.
#[test]
fn ca_cli_10_stdout_is_answer_only() {
    let api = MockApi::start(status_body("succeeded", "the final answer", ""));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .success()
        .stdout("the final answer\n")
        .stderr(predicates::str::contains("Submitted"))
        .stderr(predicates::str::contains("workspace (cloud)"))
        .stderr(predicates::str::contains("cannot see your local files"))
        .stderr(predicates::str::contains(format!(
            "continue_with={CONV_ID} web_url=https://app.example.com/c/{CONV_ID}"
        )));
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
        .stderr(predicates::str::contains("provider_error"))
        .stderr(predicates::str::contains(format!(
            "continue_with={CONV_ID} web_url=https://app.example.com/c/{CONV_ID}"
        )));
}

// CA-CLI-12: REQUIRED_APPROVAL exits 5 with the approval URL on stderr.
#[test]
fn ca_cli_12_required_approval_exits_5() {
    let api = MockApi::start(status_body("required_approval", "", ""));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .code(5)
        .stderr(predicates::str::contains("Approve"))
        .stderr(predicates::str::contains(format!(
            "continue_with={CONV_ID} web_url=https://app.example.com/c/{CONV_ID}"
        )));
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

#[test]
fn ca_cont_12_continue_requires_a_value() {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token")
        .args(["chat", "-p", "hello", "--continue"])
        .assert()
        .code(2);
}

#[test]
fn continue_run_id_resolves_then_submits_same_conversation() {
    let api = RecordingApi::start(vec![
        ("200 OK", status_body("succeeded", "old answer", "")),
        ("202 Accepted", submitted_body()),
    ]);
    cli(&api.base_url)
        .args([
            "chat",
            "-p",
            "follow up",
            "--continue",
            RUN_ID,
            "--no-wait",
            "--json",
        ])
        .assert()
        .success();

    let requests = api.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with(&format!("GET /api/v1/cli/runs/{RUN_ID} ")));
    assert!(requests[1].starts_with("POST /api/v1/cli/runs "));
    assert!(requests[1].contains(&format!(r#""conversation_id":"{CONV_ID}""#)));
}

#[test]
fn continue_conversation_id_falls_through_404_then_submits_it() {
    let api = RecordingApi::start(vec![
        (
            "404 Not Found",
            serde_json::json!({"detail": "Run not found"}).to_string(),
        ),
        ("202 Accepted", submitted_body()),
    ]);
    cli(&api.base_url)
        .args([
            "chat",
            "-p",
            "follow up",
            "--continue",
            CONV_ID,
            "--no-wait",
        ])
        .assert()
        .success();

    let requests = api.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with(&format!("GET /api/v1/cli/runs/{CONV_ID} ")));
    assert!(requests[1].contains(&format!(r#""conversation_id":"{CONV_ID}""#)));
}

#[test]
fn continue_unknown_id_reports_conversation_or_run_not_found() {
    let api = RecordingApi::start(vec![
        (
            "404 Not Found",
            serde_json::json!({"detail": "Run not found"}).to_string(),
        ),
        (
            "404 Not Found",
            serde_json::json!({"detail": "Conversation not found"}).to_string(),
        ),
    ]);
    cli(&api.base_url)
        .args(["chat", "-p", "follow up", "--continue", RUN_ID, "--no-wait"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("conversation or run not found"));
    assert_eq!(api.requests().len(), 2);
}

#[test]
fn ca_cont_8_no_wait_prints_submitted_envelope_without_polling() {
    let api = RecordingApi::start(vec![("202 Accepted", submitted_body())]);
    let output = cli(&api.base_url)
        .args(["chat", "-p", "fan out", "--no-wait", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["run_id"], RUN_ID);
    assert_eq!(value["conversation_id"], CONV_ID);
    assert_eq!(value["status"], "pending");
    assert_eq!(api.requests().len(), 1);
}

#[test]
fn ca_cli_selection_submits_default_pro() {
    let api = RecordingApi::start(vec![("202 Accepted", submitted_body())]);
    cli(&api.base_url)
        .args(["chat", "-p", "hello", "--no-wait", "--json"])
        .assert()
        .success();
    let requests = api.requests();
    let (_, body) = requests[0].split_once("\r\n\r\n").unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["selection"]["option_id"], "mode:pro");
    assert!(body["selection"]["thinking_effort"].is_null());
}

#[test]
fn ca_cont_9_status_wait_polls_until_terminal() {
    let api = RecordingApi::start(vec![
        ("200 OK", status_body("pending", "", "")),
        ("200 OK", status_body("succeeded", "collected answer", "")),
    ]);
    cli(&api.base_url)
        .args(["chat", "status", RUN_ID, "--wait"])
        .assert()
        .success()
        .stdout("collected answer\n");
    assert_eq!(api.requests().len(), 2);
}

fn list_body() -> String {
    format!(
        r#"[{{"run_id":"{RUN_ID}","conversation_id":"{CONV_ID}","status":"succeeded","prompt_preview":"audit production","created_at":"2026-08-06T10:00:00Z","web_url":"https://app.example.com/c/{CONV_ID}"}}]"#
    )
}

#[test]
fn chat_ls_renders_table_and_filter_query() {
    let api = RecordingApi::start(vec![("200 OK", list_body())]);
    cli(&api.base_url)
        .args(["chat", "ls", "--conversation", CONV_ID, "--limit", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("audit production"))
        .stdout(predicates::str::contains(RUN_ID));
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains(&format!("conversation_id={CONV_ID}")));
    assert!(requests[0].contains("limit=1"));
}

#[test]
fn chat_ls_json_and_empty_list_exit_zero() {
    let populated = MockApi::start(list_body());
    let output = cli(&populated.base_url)
        .args(["chat", "ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value[0]["prompt_preview"], "audit production");

    let empty = MockApi::start("[]".into());
    cli(&empty.base_url)
        .args(["chat", "ls"])
        .assert()
        .success()
        .stdout("");
}

// ---------------------------------------------------------------------------
// `update` self-update (CA-UP-*) — driven through axoupdater's test hooks:
//   - AXOUPDATER_CONFIG_PATH  → where the install receipt is looked up
//   - CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL → the GitHub API base (mock)
// The receipt's install_prefix must match the spawned binary's real location
// (axoupdater verifies the exe came from the receipt before updating).
// ---------------------------------------------------------------------------

/// The axoupdater app name; must equal the receipt filename stem and the
/// release-asset prefix the installer looks for.
const UPDATE_APP: &str = "cloudthinker-cli";

/// Release JSON for one GitHub API release object, with the installer asset
/// pointing back at the mock server.
fn release_object(mock_base: &str, tag: &str, prerelease: bool) -> String {
    format!(
        r#"{{"tag_name":"{tag}","name":"{tag}","url":"{mock_base}/releases/{tag}","assets":[{{"name":"{UPDATE_APP}-installer.sh","url":"{mock_base}/installer.sh","browser_download_url":"{mock_base}/installer.sh"}}],"prerelease":{prerelease}}}"#
    )
}

/// Release JSON for the GitHub API `releases/latest` endpoint: the newest
/// stable release.
fn release_body(mock_base: &str, tag: &str) -> String {
    release_object(mock_base, tag, false)
}

/// Release-list JSON for the GitHub API `/releases` endpoint.
fn releases_list_body(mock_base: &str, releases: &[(&str, bool)]) -> String {
    let items: Vec<String> = releases
        .iter()
        .map(|(tag, prerelease)| release_object(mock_base, tag, *prerelease))
        .collect();
    format!("[{}]", items.join(","))
}

/// Writes a cargo-dist install receipt for the spawned binary's real location
/// and returns its directory (to pass as AXOUPDATER_CONFIG_PATH). `tag` makes
/// the directory unique per test so parallel runs never clobber each other.
fn write_receipt(tag: &str, version: &str, install_prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ct-update-receipt-{}-{tag}-{}",
        std::process::id(),
        version.replace('.', "_")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let receipt = format!(
        r#"{{"binaries":["cloudthinker"],"install_prefix":"{install_prefix}","provider":{{"source":"cargo-dist","version":"0.32.0"}},"source":{{"release_type":"github","owner":"cloudthinker-ai","name":"{UPDATE_APP}","app_name":"{UPDATE_APP}"}},"version":"{version}"}}"#
    );
    std::fs::write(dir.join(format!("{UPDATE_APP}-receipt.json")), receipt).unwrap();
    dir
}

/// The real install prefix the exe-matches-receipt check expects: the parent
/// of the built binary, canonicalized (axoupdater canonicalizes both sides).
fn real_install_prefix() -> String {
    let bin = assert_cmd::cargo::cargo_bin("cloudthinker");
    bin.parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

/// Canned server for `update` tests: serves the releases API under /api/v3
/// (axoupdater's GHE base-url override appends that prefix) and the installer
/// script the updater downloads and executes.
struct MockReleases {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    listener: Option<TcpListener>,
}

/// What one canned request answers: status line, body, content type. The
/// argument is the request's first line.
type Route = Box<dyn Fn(&str) -> (String, String, &'static str) + Send>;

impl MockReleases {
    /// One stable release, no prerelease: the release list and
    /// `releases/latest` both resolve to it, so either update strategy sees
    /// the same single release.
    fn start(tag: &str, installer_script: String) -> Self {
        let mock = MockReleases::bind();
        let release_body = release_body(&mock.base_url, tag);
        let list_body = releases_list_body(&mock.base_url, &[(tag, false)]);
        mock.serve(Box::new(move |path| {
            if path.contains("/releases/latest") {
                (
                    "200 OK".to_string(),
                    release_body.clone(),
                    "application/json",
                )
            } else if path.contains("/api/v3/repos/") {
                ("200 OK".to_string(), list_body.clone(), "application/json")
            } else if path.contains("/installer.sh") {
                ("200 OK".to_string(), installer_script.clone(), "text/plain")
            } else {
                ("404 Not Found".to_string(), String::new(), "text/plain")
            }
        }))
    }

    /// Both release tracks on one server: `releases/latest` serves the stable
    /// object while the list serves stable plus prerelease, so the stable and
    /// dev channels resolve against realistic GitHub responses.
    fn start_two(stable_tag: &str, prerelease_tag: &str, installer_script: String) -> Self {
        let mock = MockReleases::bind();
        let stable_body = release_object(&mock.base_url, stable_tag, false);
        let list_body = releases_list_body(
            &mock.base_url,
            &[(stable_tag, false), (prerelease_tag, true)],
        );
        mock.serve(Box::new(move |path| {
            if path.contains("/releases/latest") {
                (
                    "200 OK".to_string(),
                    stable_body.clone(),
                    "application/json",
                )
            } else if path.contains("/api/v3/repos/") {
                ("200 OK".to_string(), list_body.clone(), "application/json")
            } else if path.contains("/installer.sh") {
                ("200 OK".to_string(), installer_script.clone(), "text/plain")
            } else {
                ("404 Not Found".to_string(), String::new(), "text/plain")
            }
        }))
    }

    fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        Self {
            base_url: format!("http://{addr}"),
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            listener: Some(listener),
        }
    }

    /// `installer_script` is served as the release's installer; give it a
    /// side effect (e.g. touch a marker) to prove it actually executed.
    fn serve(mut self, route: Route) -> Self {
        let listener = self.listener.take().unwrap();
        let stop_thread = self.stop.clone();
        self.handle = Some(std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let data = read_request(&mut socket);
                        let request = String::from_utf8_lossy(&data);
                        let first_line = request.lines().next().unwrap_or_default();
                        let (status, body, content_type) = route(first_line.trim());
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
        }));
        self
    }
}

impl Drop for MockReleases {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the real binary with the axoupdater test hooks pointed at the mock.
fn update_cli(receipt_dir: &std::path::Path, releases: &MockReleases, marker: &str) -> Command {
    let mut cmd = Command::cargo_bin("cloudthinker").unwrap();
    cmd.env("AXOUPDATER_CONFIG_PATH", receipt_dir)
        .env(
            "CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL",
            &releases.base_url,
        )
        .env("CT_UPDATE_TEST_MARKER", marker)
        .env("HOME", fresh_home("update"))
        .env_remove("CLOUDTHINKER_URL")
        .env_remove("NO_COLOR");
    cmd
}

/// Fake installer: writes a marker file whose path arrives via env, so tests
/// can prove the updater downloaded AND executed the installer.
const INSTALLER_NOISE: &str = "installing to /home/dev/.local/bin";

fn marker_installer_script() -> String {
    format!(
        "#!/bin/sh\ntouch \"$CT_UPDATE_TEST_MARKER\"\necho '{INSTALLER_NOISE}'\necho '{INSTALLER_NOISE}' >&2\n"
    )
}

fn marker_path(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("ct-update-marker-{}-{name}", std::process::id()))
        .to_str()
        .unwrap()
        .to_string()
}

// CA-UP-1: an update is available → the installer runs and the outcome line
// lands on stdout, exit 0.
#[test]
fn ca_up_1_update_available_installs_and_exits_0() {
    let marker = marker_path("up1");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up1", "0.1.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .arg("update")
        .assert()
        .success()
        .stdout("Updated cloudthinker from 0.1.0 to 0.2.0\n")
        .stderr(predicates::str::contains(INSTALLER_NOISE).not());
    assert!(
        std::path::Path::new(&marker).exists(),
        "installer must have run"
    );
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-15: the installer runs silently, but a failed install still shows
// what the installer said.
#[test]
fn ca_up_15_a_failed_install_shows_the_installer_output() {
    let marker = marker_path("up15");
    let releases = MockReleases::start(
        "v0.2.0",
        "#!/bin/sh\necho 'disk is full' >&2\nexit 1\n".to_string(),
    );
    let receipt_dir = write_receipt("up15", "0.1.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .arg("update")
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("update failed"))
        .stderr(predicates::str::contains("disk is full"));
}

// CA-UP-2: already on the latest release → no installer run, exit 0.
#[test]
fn ca_up_2_already_up_to_date_exits_0() {
    let marker = marker_path("up2");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up2", "0.2.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .arg("update")
        .assert()
        .success()
        .stdout("cloudthinker is already up to date\n");
    assert!(
        !std::path::Path::new(&marker).exists(),
        "installer must NOT run when up to date"
    );
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-3: no install receipt → refuse with the manual reinstall command,
// exit 1, nothing on stdout.
#[test]
fn ca_up_3_missing_receipt_refuses_without_stdout() {
    let marker = marker_path("up3");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let empty_dir =
        std::env::temp_dir().join(format!("ct-update-receipt-{}-empty", std::process::id()));
    std::fs::create_dir_all(&empty_dir).unwrap();

    update_cli(&empty_dir, &releases, &marker)
        .arg("update")
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains("cannot self-update this installation"))
        .stderr(predicates::str::contains(
            "https://github.com/cloudthinker-ai/cloudthinker-cli/releases/latest/download/cloudthinker-cli-installer.sh",
        ));
    assert!(!std::path::Path::new(&marker).exists());
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-4: `--json` on an update prints exactly one envelope, exit 0.
#[test]
fn ca_up_4_json_update_available_emits_envelope() {
    let marker = marker_path("up4");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up4", "0.1.0", &real_install_prefix());

    let assert = update_cli(&receipt_dir, &releases, &marker)
        .args(["update", "--json"])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let envelope: serde_json::Value =
        serde_json::from_slice(&stdout).expect("stdout must be one JSON envelope");
    assert_eq!(envelope["updated"], true);
    assert_eq!(envelope["old_version"], "0.1.0");
    assert_eq!(envelope["new_version"], "0.2.0");
    assert!(
        std::path::Path::new(&marker).exists(),
        "installer must have run"
    );
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-5: `--json` when already current reports updated:false, exit 0.
#[test]
fn ca_up_5_json_up_to_date_emits_envelope() {
    let marker = marker_path("up5");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up5", "0.2.0", &real_install_prefix());

    let assert = update_cli(&receipt_dir, &releases, &marker)
        .args(["update", "--json"])
        .assert()
        .success();
    let stdout = assert.get_output().stdout.clone();
    let envelope: serde_json::Value =
        serde_json::from_slice(&stdout).expect("stdout must be one JSON envelope");
    assert_eq!(envelope["updated"], false);
    assert_eq!(envelope["old_version"], serde_json::Value::Null);
    assert_eq!(envelope["new_version"], serde_json::Value::Null);
    assert!(!std::path::Path::new(&marker).exists());
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-6: `--force` reinstalls the same version, exit 0.
#[test]
fn ca_up_6_force_reinstalls_when_up_to_date() {
    let marker = marker_path("up6");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up6", "0.2.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .args(["update", "--force"])
        .assert()
        .success()
        .stdout("Updated cloudthinker from 0.2.0 to 0.2.0\n");
    assert!(
        std::path::Path::new(&marker).exists(),
        "installer must have run"
    );
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-7: a binary that does not match the receipt (different install
// prefix) is refused instead of misreported as up to date.
#[test]
fn ca_up_7_exe_mismatch_refuses() {
    let marker = marker_path("up7");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up7", "0.1.0", "/nonexistent/install/prefix");

    update_cli(&receipt_dir, &releases, &marker)
        .arg("update")
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains(
            "the running binary does not match the install receipt",
        ));
    assert!(!std::path::Path::new(&marker).exists());
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-8: axoupdater env overrides present at runtime are surfaced on
// stderr — a shared/CI environment gets a warning instead of a silent
// redirect of where the receipt is read and where the installer is fetched.
#[test]
fn ca_up_8_env_overrides_warn_on_stderr() {
    let marker = marker_path("up8");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start("v0.2.0", marker_installer_script());
    let receipt_dir = write_receipt("up8", "0.2.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .arg("update")
        .assert()
        .success()
        .stdout("cloudthinker is already up to date\n")
        .stderr(predicates::str::contains("AXOUPDATER_CONFIG_PATH"))
        .stderr(predicates::str::contains(
            "CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL",
        ));
    let _ = std::fs::remove_file(&marker);
}

#[cfg(unix)]
fn write_agent_stub(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("ct-agent-stub-{}-{name}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("cloudthinker-agent");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         echo \"argv: $*\"\n\
         echo \"url: $CLOUDTHINKER_URL\"\n\
         if [ -n \"$CLOUDTHINKER_WORKSPACE\" ]; then echo 'workspace: present'; else echo 'workspace: absent'; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn agent_with_revoked_credential_is_auth_error_without_stdout() {
    let api = MockApi::start_with_status(
        "401 Unauthorized",
        r#"{"error":{"code":"unauthorized","message":"expired","retryable":false},"detail":"expired"}"#.into(),
    );

    cli(&api.base_url)
        .arg("agent")
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicates::str::contains("not authenticated"))
        .stderr(predicates::str::contains(
            "The credential in CLOUDTHINKER_TOKEN is rejected. Replace it, or unset it and run `cloudthinker login`.",
        ));
}

#[cfg(unix)]
#[test]
fn agent_execs_the_override_binary_with_the_argument_and_env_contract() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let stub = write_agent_stub("exec");

    cli(&api.base_url)
        .env("CLOUDTHINKER_AGENT_BIN", &stub)
        .args(["agent", "-p", "hello", "--model", "cloudthinker/pro"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "argv: -p hello --model cloudthinker/pro",
        ))
        .stdout(predicates::str::contains(format!(
            "url: {}\n",
            api.base_url
        )))
        .stdout(predicates::str::contains("workspace: absent"))
        .stderr(predicates::str::contains("CLOUDTHINKER_AGENT_BIN"));

    let _ = std::fs::remove_dir_all(stub.parent().unwrap());
}

#[cfg(unix)]
#[test]
fn ca_ad_7_wrapper_timing_is_opt_in_and_stays_off_stdout() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let stub = write_agent_stub("timing");
    for enabled in [false, true] {
        let mut command = cli(&api.base_url);
        command.env("CLOUDTHINKER_AGENT_BIN", &stub).arg("agent");
        if enabled {
            command.env("CLOUDTHINKER_TIMING", "1");
        } else {
            command.env_remove("CLOUDTHINKER_TIMING");
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stdout.contains("[cloudthinker timing]"));
        assert_eq!(stderr.contains("wrapper.install_check"), enabled);
        assert!(!stderr.contains("test-access-token"));
    }
    std::fs::remove_dir_all(stub.parent().unwrap()).unwrap();
}

#[cfg(unix)]
#[test]
fn agent_reports_a_missing_override_binary_without_stdout() {
    let api = MockApi::start(WHOAMI_BODY.into());

    cli(&api.base_url)
        .env("CLOUDTHINKER_AGENT_BIN", "/nonexistent/cloudthinker-agent")
        .arg("agent")
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicates::str::contains(
            "could not run /nonexistent/cloudthinker-agent",
        ));
}

#[test]
fn workspace_from_the_environment_still_conflicts_with_the_token_variable() {
    let api = MockApi::start(WHOAMI_BODY.into());

    cli(&api.base_url)
        .env("CLOUDTHINKER_WORKSPACE", "Production")
        .arg("whoami")
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains(
            "--workspace cannot be used with CLOUDTHINKER_TOKEN",
        ));
}

// ---------------------------------------------------------------------------
// Start-up release offer (CA-UP-9/10) — `agent` runs `update::offer_on_start`
// before anything else, and that check only speaks on a TTY, so these drive
// the real binary through a pty. `CLOUDTHINKER_AGENT_BIN` keeps the run inside
// the stub once the check is done.
// ---------------------------------------------------------------------------

/// This binary's version, the one the offer compares a release against.
const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Upper bound on what a pty run may return, so a child that never stops
/// writing fails the test instead of growing the harness without limit.
const MAX_PTY_OUTPUT: u64 = 64 * 1024;

/// A release tag strictly newer than `RUNNING_VERSION`, whatever it is today.
fn newer_tag() -> String {
    let major: u64 = RUNNING_VERSION
        .split('.')
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    format!("v{major}.0.0")
}

/// Run `cloudthinker agent` on a pty against both mocks and return everything
/// it wrote to the terminal. `n` is queued on stdin so an offer is declined
/// rather than installed; when no offer appears the byte is simply never read.
#[cfg(unix)]
fn agent_on_a_tty(
    api: &MockApi,
    releases: &MockReleases,
    receipt_dir: &std::path::Path,
    home: &std::path::Path,
) -> String {
    agent_on_a_tty_answering(api, releases, receipt_dir, home, b"n\n")
}

#[cfg(unix)]
fn agent_on_a_tty_answering(
    api: &MockApi,
    releases: &MockReleases,
    receipt_dir: &std::path::Path,
    home: &std::path::Path,
    answer: &[u8],
) -> String {
    binary_on_a_tty(
        &assert_cmd::cargo::cargo_bin("cloudthinker"),
        &["agent"],
        answer,
        api,
        releases,
        receipt_dir,
        home,
    )
}

fn fresh_home(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ct-home-{}-{name}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cache_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".cloudthinker").join("update-check.json")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn seed_cache(home: &std::path::Path, latest: &str, checked_at_unix: u64) {
    let path = cache_file(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::json!({
            "channel": "dev",
            "latest_version": latest,
            "checked_at_unix": checked_at_unix,
        })
        .to_string(),
    )
    .unwrap();
}

fn read_cache(home: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(cache_file(home)).unwrap()).unwrap()
}

/// Run `binary` with `args` on a pty against both mocks, queue `answer` on
/// stdin for the offer prompt, and return everything it wrote to the terminal.
#[cfg(unix)]
fn binary_on_a_tty(
    binary: &std::path::Path,
    args: &[&str],
    answer: &[u8],
    api: &MockApi,
    releases: &MockReleases,
    receipt_dir: &std::path::Path,
    home: &std::path::Path,
) -> String {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let stub = write_agent_stub("tty");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(binary);
    cmd.args(args);
    cmd.env("CLOUDTHINKER_TOKEN", "test-access-token");
    cmd.env("CLOUDTHINKER_URL", &api.base_url);
    cmd.env("CLOUDTHINKER_AGENT_BIN", &stub);
    cmd.env("AXOUPDATER_CONFIG_PATH", receipt_dir);
    cmd.env(
        "CLOUDTHINKER_CLI_INSTALLER_GHE_BASE_URL",
        &releases.base_url,
    );
    cmd.env("NO_COLOR", "1");
    cmd.env("HOME", home);
    cmd.env_remove("CLOUDTHINKER_NO_UPDATE_CHECK");
    cmd.env_remove("CLOUDTHINKER_WORKSPACE");

    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut writer = pair.master.take_writer().unwrap();
    writer.write_all(answer).unwrap();
    drop(writer);

    let mut killer = child.clone_killer();
    let finished = Arc::new(AtomicBool::new(false));
    let watchdog_flag = finished.clone();
    let watchdog = std::thread::spawn(move || {
        for _ in 0..300 {
            if watchdog_flag.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = killer.kill();
    });

    let reader = pair.master.try_clone_reader().unwrap();
    drop(pair.master);
    let mut output = Vec::new();
    let _ = reader.take(MAX_PTY_OUTPUT).read_to_end(&mut output);
    let _ = child.wait();
    finished.store(true, Ordering::SeqCst);
    watchdog.join().unwrap();

    let _ = std::fs::remove_dir_all(stub.parent().unwrap());
    assert!(
        (output.len() as u64) < MAX_PTY_OUTPUT,
        "the pty run wrote at least {MAX_PTY_OUTPUT} bytes without finishing"
    );
    String::from_utf8_lossy(&output).replace("\r\n", "\n")
}

// CA-UP-9: a genuinely newer release is offered on the terminal, naming both
// versions and the dev channel the local origin resolves to, and declining it
// still starts the session.
#[cfg(unix)]
#[test]
fn ca_up_9_a_newer_release_is_offered_and_declining_still_starts_the_agent() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let tag = newer_tag();
    let releases = MockReleases::start(&tag, marker_installer_script());
    let receipt_dir = write_receipt("up6", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("up9");
    seed_cache(&home, tag.trim_start_matches('v'), now_unix());

    let output = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        output.contains(&format!(
            "cloudthinker {} is available on the dev channel (you have {RUNNING_VERSION})",
            tag.trim_start_matches('v')
        )),
        "expected the offer, got:\n{output}"
    );
    assert!(
        output.contains("Install it now? [y/N]"),
        "expected the prompt, got:\n{output}"
    );
    assert!(
        output.contains("argv: "),
        "declining must still start the agent, got:\n{output}"
    );
}

#[cfg(unix)]
#[test]
fn agent_help_reaches_the_agent_without_a_release_offer() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let releases = MockReleases::start(&newer_tag(), marker_installer_script());
    let receipt_dir = write_receipt("help", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("help");
    seed_cache(&home, newer_tag().trim_start_matches('v'), now_unix());

    let output = binary_on_a_tty(
        &assert_cmd::cargo::cargo_bin("cloudthinker"),
        &["agent", "--help"],
        b"n\n",
        &api,
        &releases,
        &receipt_dir,
        &home,
    );

    assert!(
        !output.contains("Install it now?"),
        "a help request must not offer a release, got:\n{output}"
    );
    assert!(
        output.contains("argv: --help"),
        "the agent must receive --help, got:\n{output}"
    );
}

/// A private copy of the built binary that a fake installer may replace, plus
/// the receipt that claims it. The real install location must stay untouched.
#[cfg(unix)]
fn installed_copy(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ct-installed-copy-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let binary = dir.join("cloudthinker");
    let copied = std::process::Command::new("cp")
        .arg(assert_cmd::cargo::cargo_bin("cloudthinker"))
        .arg(&binary)
        .status()
        .unwrap();
    assert!(copied.success(), "cp of the built binary failed: {copied}");
    let receipt_dir = write_receipt(tag, RUNNING_VERSION, dir.to_str().unwrap());
    (binary, receipt_dir)
}

/// Fake installer that does what the real one does to the binary: writes the
/// new release beside it and renames it over the old path. The "release" is a
/// script that reports the argv and the update-check opt-out it received.
#[cfg(unix)]
fn replacing_installer_script(binary: &std::path::Path) -> String {
    let path = binary.to_str().unwrap();
    format!(
        "#!/bin/sh\n\
         cat > \"{path}.new\" <<'STUB'\n\
         #!/bin/sh\n\
         echo \"reexec argv: $*\"\n\
         echo \"reexec update check: ${{CLOUDTHINKER_NO_UPDATE_CHECK:-unset}}\"\n\
         STUB\n\
         echo '{INSTALLER_NOISE}'\n\
         chmod +x \"{path}.new\"\n\
         mv \"{path}.new\" \"{path}\"\n"
    )
}

// CA-UP-11: accepting the offer hands this start to the binary the installer
// just wrote, with the original arguments, and that binary skips a second
// check. The old process must not carry on: it would run the old agent bundle,
// and on macOS its first Keychain read fails once its on-disk code changed.
#[cfg(unix)]
#[test]
fn ca_up_11_an_accepted_offer_continues_in_the_installed_binary() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let tag = newer_tag();
    let (binary, receipt_dir) = installed_copy("up11");
    let releases = MockReleases::start(&tag, replacing_installer_script(&binary));
    let home = fresh_home("up11");
    seed_cache(&home, tag.trim_start_matches('v'), now_unix());

    let output = binary_on_a_tty(
        &binary,
        &["agent", "--", "--resume"],
        b"y\n",
        &api,
        &releases,
        &receipt_dir,
        &home,
    );

    assert!(
        output.contains(&format!(
            "Updating cloudthinker to {}",
            tag.trim_start_matches('v')
        )),
        "expected the update step, got:\n{output}"
    );
    assert!(
        !output.contains(INSTALLER_NOISE),
        "the installer output must stay hidden, got:\n{output}"
    );
    assert!(
        output.contains(&format!(
            "Updated cloudthinker from {RUNNING_VERSION} to {}",
            tag.trim_start_matches('v')
        )),
        "expected the finished update before the restart, got:\n{output}"
    );
    assert!(
        output.contains("reexec argv: agent -- --resume"),
        "the installed binary must continue this start with the same arguments, got:\n{output}"
    );
    assert!(
        output.contains("reexec update check: 1"),
        "the installed binary must not offer again, got:\n{output}"
    );
    assert!(
        !output.contains("argv: --resume"),
        "the old process must not start the agent, got:\n{output}"
    );
    let _ = std::fs::remove_dir_all(binary.parent().unwrap());
}

// CA-UP-10: the release the user is already running is not offered back to
// them. `query_new_version` reports the latest release and compares nothing,
// so without our own comparison this prompts "0.5.0 is available (you have
// 0.5.0)" on every single start.
#[cfg(unix)]
#[test]
fn ca_up_10_the_running_release_is_never_offered_back() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let releases = MockReleases::start(&format!("v{RUNNING_VERSION}"), marker_installer_script());
    let receipt_dir = write_receipt("up7", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("up10");
    seed_cache(&home, RUNNING_VERSION, now_unix());

    let output = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        !output.contains("is available"),
        "the running version must not be offered, got:\n{output}"
    );
    assert!(
        !output.contains("Install it now?"),
        "no prompt is due, got:\n{output}"
    );
    assert!(
        output.contains("argv: "),
        "the agent must start, got:\n{output}"
    );
}

#[cfg(unix)]
#[test]
fn ca_up_16_a_fresh_cache_offers_without_asking_the_release_host() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let releases = MockReleases::start(&format!("v{RUNNING_VERSION}"), marker_installer_script());
    let receipt_dir = write_receipt("up16", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("up16");
    let newer = newer_tag().trim_start_matches('v').to_string();
    let checked_at = now_unix() - 60;
    seed_cache(&home, &newer, checked_at);

    let output = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        output.contains(&format!("cloudthinker {newer} is available")),
        "the cached release must be offered, got:\n{output}"
    );
    assert_eq!(
        read_cache(&home)["checked_at_unix"],
        checked_at,
        "a fresh cache must not be refreshed"
    );
    assert_eq!(read_cache(&home)["latest_version"], newer.as_str());
}

#[cfg(unix)]
#[test]
fn ca_up_17_an_empty_cache_is_filled_in_the_background_and_offered_next_start() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let tag = newer_tag();
    let newer = tag.trim_start_matches('v');
    let releases = MockReleases::start(&tag, marker_installer_script());
    let receipt_dir = write_receipt("up17", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("up17");

    let first = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        !first.contains("is available") && first.contains("argv: "),
        "the first start must not wait for an offer, got:\n{first}"
    );
    assert_eq!(read_cache(&home)["latest_version"], newer);
    assert_eq!(read_cache(&home)["channel"], "dev");

    let second = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        second.contains(&format!("cloudthinker {newer} is available")),
        "the next start must offer what the refresh found, got:\n{second}"
    );
}

#[cfg(unix)]
#[test]
fn ca_up_18_a_skipped_version_stays_quiet_until_a_newer_one_ships() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let tag = newer_tag();
    let skipped = tag.trim_start_matches('v').to_string();
    let releases = MockReleases::start(&tag, marker_installer_script());
    let receipt_dir = write_receipt("up18", RUNNING_VERSION, &real_install_prefix());
    let home = fresh_home("up18");
    seed_cache(&home, &skipped, now_unix());

    let skipping = agent_on_a_tty_answering(&api, &releases, &receipt_dir, &home, b"s\n");

    assert!(
        skipping.contains("(s skips this version)") && skipping.contains("argv: "),
        "skipping must still start the agent, got:\n{skipping}"
    );
    assert_eq!(read_cache(&home)["dismissed_version"], skipped.as_str());

    let quiet = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        !quiet.contains("is available"),
        "a skipped version must not be offered again, got:\n{quiet}"
    );

    let major: u64 = skipped.split('.').next().unwrap().parse().unwrap();
    let newer = format!("{}.0.0", major + 1);
    seed_cache(&home, &newer, now_unix());
    let mut cache = read_cache(&home);
    cache["dismissed_version"] = serde_json::Value::String(skipped.clone());
    std::fs::write(cache_file(&home), cache.to_string()).unwrap();

    let offered = agent_on_a_tty(&api, &releases, &receipt_dir, &home);

    assert!(
        offered.contains(&format!("cloudthinker {newer} is available")),
        "a release newer than the skipped one must be offered, got:\n{offered}"
    );
}

// CA-UP-12: from the production origin the updater skips a newer prerelease
// and installs the newest stable instead.
#[test]
fn ca_up_12_prod_origin_skips_a_newer_prerelease() {
    let marker = marker_path("up12");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start_two("v0.2.0", "v0.3.0-dev.1", marker_installer_script());
    let receipt_dir = write_receipt("up12", "0.1.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .env("CLOUDTHINKER_URL", "https://app.cloudthinker.io")
        .arg("update")
        .assert()
        .success()
        .stdout("Updated cloudthinker from 0.1.0 to 0.2.0\n");
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-13: from a dev origin the updater takes the prerelease — the newest
// release overall — and the run still exits 0.
#[test]
fn ca_up_13_dev_origin_follows_the_prerelease() {
    let marker = marker_path("up13");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start_two("v0.2.0", "v0.3.0-dev.1", marker_installer_script());
    let receipt_dir = write_receipt("up13", "0.1.0", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .env("CLOUDTHINKER_URL", "https://dev.cloudthinker.io")
        .arg("update")
        .assert()
        .success()
        .stdout("Updated cloudthinker from 0.1.0 to 0.3.0-dev.1\n");
    assert!(
        std::path::Path::new(&marker).exists(),
        "the dev-channel installer must have run"
    );
    let _ = std::fs::remove_file(&marker);
}

// CA-UP-14: after promotion the stable tag outruns the dev build the user is
// on, so a dev-origin install is pulled up to stable.
#[test]
fn ca_up_14_promotion_pulls_a_dev_user_up_to_stable() {
    let marker = marker_path("up14");
    let _ = std::fs::remove_file(&marker);
    let releases = MockReleases::start_two("v0.3.0", "v0.3.0-dev.1", marker_installer_script());
    let receipt_dir = write_receipt("up14", "0.3.0-dev.1", &real_install_prefix());

    update_cli(&receipt_dir, &releases, &marker)
        .env("CLOUDTHINKER_URL", "https://dev.cloudthinker.io")
        .arg("update")
        .assert()
        .success()
        .stdout("Updated cloudthinker from 0.3.0-dev.1 to 0.3.0\n");
    let _ = std::fs::remove_file(&marker);
}

#[test]
fn ca_cli_skill_exports_bundled_modules_offline() {
    for (topic, file) in [
        ("index", "SKILL.md"),
        ("auth", "auth.md"),
        ("chat", "chat.md"),
        ("review", "review.md"),
        ("worker", "worker.md"),
    ] {
        let expected = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("skills/cloudthinker-cli")
                .join(file),
        )
        .unwrap();
        let mut command = Command::cargo_bin("cloudthinker").unwrap();
        command
            .env("CLOUDTHINKER_URL", "http://127.0.0.1:1")
            .env_remove("CLOUDTHINKER_TOKEN")
            .env_remove("CLOUDTHINKER_WORKSPACE")
            .timeout(Duration::from_secs(5))
            .arg("--skill");
        if topic != "index" {
            command.arg(topic);
        }
        command.assert().success().stdout(expected).stderr("");
    }
}

#[test]
fn ca_cli_skill_rejects_unknown_topics_and_execution() {
    for args in [
        vec!["--skill", "missing"],
        vec!["--skill=index", "whoami"],
        vec!["--skill=chat", "chat", "-p", "do not submit"],
    ] {
        Command::cargo_bin("cloudthinker")
            .unwrap()
            .env("CLOUDTHINKER_URL", "http://127.0.0.1:1")
            .args(args)
            .timeout(Duration::from_secs(5))
            .assert()
            .code(2)
            .stdout("");
    }
}

const OUTPOST_ID: &str = "33333333-3333-4333-8333-333333333333";

fn outpost_json(name: &str, target_id: Option<&str>) -> String {
    let target = target_id.map_or_else(|| "null".to_string(), |id| format!("\"{id}\""));
    format!(
        r#"{{"availability":"available","capabilities":["shell"],"kind":"outpost","name":"{name}","scope":"personal","target_id":{target}}}"#
    )
}

fn worker_state_dir(config_home: &std::path::Path, origin: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    #[cfg(target_os = "macos")]
    let config_home = config_home.join("Library/Application Support");
    config_home
        .join("cloudthinker/worker-state")
        .join(format!("{:x}", Sha256::digest(origin.as_bytes())))
}

fn worker_cli(base_url: &str, config_home: &std::path::Path) -> Command {
    let mut command = cli(base_url);
    command
        .env("HOME", config_home)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("CLOUDTHINKER_OUTPOST_ID")
        .env_remove("CLOUDTHINKER_WORKER_TOKEN")
        .timeout(Duration::from_secs(20));
    command
}

#[test]
fn ca_wo_01_outpost_create_stores_the_exchanged_credential_and_prints_the_start_command() {
    let home = tempfile::tempdir().unwrap();
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "201 Created",
            format!(
                r#"{{"registration":{{"expires_at":"2026-09-14T00:00:00Z","reference":"one-time-ref"}},"target":{}}}"#,
                outpost_json("build", Some(OUTPOST_ID))
            ),
        ),
        (
            "200 OK",
            format!(
                r#"{{"credential":"worker-token","credential_generation":1,"expires_at":"2026-09-14T00:00:00Z","target_id":"{OUTPOST_ID}"}}"#
            ),
        ),
    ]);

    worker_cli(&api.base_url, home.path())
        .args(["worker", "outpost", "create", "build"])
        .assert()
        .success()
        .stdout(format!(
            "Created outpost build; start it with cloudthinker worker start --outpost {OUTPOST_ID} --workdir <directory>\n"
        ));

    let requests = api.requests();
    assert!(
        requests[1].contains("POST /api/v1/workspaces/")
            && requests[1].contains("executor-targets"),
        "{requests:?}"
    );
    assert!(
        requests[2].contains("POST /api/v1/executor-workers/exchange"),
        "{requests:?}"
    );
    let stored = std::fs::read_to_string(
        worker_state_dir(home.path(), &api.base_url).join(format!("{OUTPOST_ID}.json")),
    )
    .unwrap();
    assert!(stored.contains("worker-token"), "{stored}");
}

#[test]
fn ca_wo_21_outpost_ls_hides_managed_execution_and_sanitizes_the_name() {
    let home = tempfile::tempdir().unwrap();
    let hostile = concat!("deploy", "\\u001b", "]0;owned", "\\u0007");
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "200 OK",
            format!(
                "[{},{},{}]",
                outpost_json("managed", None),
                outpost_json("build", Some(OUTPOST_ID)),
                outpost_json(hostile, Some("44444444-4444-4444-8444-444444444444")),
            ),
        ),
    ]);

    worker_cli(&api.base_url, home.path())
        .args(["worker", "outpost", "ls"])
        .assert()
        .success()
        .stdout("build  available\ndeploy]0;owned  available\n");
}

#[test]
fn ca_wo_21_outpost_ls_json_emits_only_the_selectable_outposts() {
    let home = tempfile::tempdir().unwrap();
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "200 OK",
            format!(
                "[{},{}]",
                outpost_json("managed", None),
                outpost_json("build", Some(OUTPOST_ID))
            ),
        ),
    ]);

    let output = worker_cli(&api.base_url, home.path())
        .args(["worker", "outpost", "ls", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let entries = value.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["target_id"], OUTPOST_ID);
}

#[test]
fn ca_wo_22_outpost_archive_resolves_the_name_then_archives_that_target() {
    let home = tempfile::tempdir().unwrap();
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "200 OK",
            format!("[{}]", outpost_json("build", Some(OUTPOST_ID))),
        ),
        ("204 No Content", String::new()),
    ]);

    worker_cli(&api.base_url, home.path())
        .args(["worker", "outpost", "archive", "build"])
        .assert()
        .success()
        .stdout("Archived outpost build\n");

    let requests = api.requests();
    assert!(
        requests[2].contains(&format!("/executor-targets/{OUTPOST_ID}")),
        "{requests:?}"
    );
}

#[test]
fn ca_wo_22_outpost_archive_of_an_unknown_name_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "200 OK",
            format!("[{}]", outpost_json("build", Some(OUTPOST_ID))),
        ),
    ]);

    worker_cli(&api.base_url, home.path())
        .args(["worker", "outpost", "archive", "release"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains("outpost not found"));
}

#[test]
fn ca_wo_21_worker_status_reports_one_outpost_availability() {
    let home = tempfile::tempdir().unwrap();
    let api = RecordingApi::start(vec![
        ("200 OK", WHOAMI_BODY.into()),
        (
            "200 OK",
            format!("[{}]", outpost_json("build", Some(OUTPOST_ID))),
        ),
    ]);

    worker_cli(&api.base_url, home.path())
        .args(["worker", "status", "--outpost", OUTPOST_ID])
        .assert()
        .success()
        .stdout("build: available\n");
}

const SERVICE_ORIGIN: &str = "https://app.example.com:443";

#[cfg(unix)]
fn private_dir(path: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
    path.to_path_buf()
}

#[cfg(unix)]
fn store_worker_credential(config_home: &std::path::Path, target_id: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir(&worker_state_dir(config_home, SERVICE_ORIGIN));
    let path = dir.join(format!("{target_id}.json"));
    std::fs::write(
        &path,
        format!(r#"{{"target_id":"{target_id}","name":"build","token":"worker-token"}}"#),
    )
    .unwrap();
    std::fs::set_permissions(&path, PermissionsExt::from_mode(0o600)).unwrap();
}

#[cfg(unix)]
fn fake_service_manager(root: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = private_dir(&root.join("bin"));
    let manager = bin.join("systemctl");
    std::fs::write(
        &manager,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&manager, PermissionsExt::from_mode(0o700)).unwrap();
    bin
}

#[cfg(unix)]
fn service_cli(config_home: &std::path::Path, bin: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("cloudthinker").unwrap();
    command
        .env("HOME", config_home)
        .env("XDG_CONFIG_HOME", config_home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("CLOUDTHINKER_TOKEN")
        .env_remove("CLOUDTHINKER_WORKSPACE")
        .env_remove("CLOUDTHINKER_OUTPOST_ID")
        .timeout(Duration::from_secs(20))
        .args(["--url", "https://app.example.com", "worker", "service"]);
    command
}

#[cfg(target_os = "linux")]
#[test]
fn ca_wo_41_repeating_one_service_install_is_unchanged_and_a_different_one_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let home = private_dir(&root.path().join("home"));
    let workdir = private_dir(&root.path().join("work"));
    let log = root.path().join("manager.log");
    let bin = fake_service_manager(root.path(), &log);
    private_dir(&home.join("systemd/user"));
    store_worker_credential(&home, OUTPOST_ID);
    let workdir = workdir.to_string_lossy().into_owned();

    service_cli(&home, &bin)
        .args([
            "install",
            "--outpost",
            OUTPOST_ID,
            "--workdir",
            &workdir,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"state\": \"installed\""));

    service_cli(&home, &bin)
        .args([
            "install",
            "--outpost",
            OUTPOST_ID,
            "--workdir",
            &workdir,
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"state\": \"unchanged\""));

    service_cli(&home, &bin)
        .args([
            "install",
            "--outpost",
            OUTPOST_ID,
            "--workdir",
            &workdir,
            "--concurrency",
            "8",
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicates::str::contains(
            "a worker service already exists with different settings",
        ));

    let unit = std::fs::read_dir(home.join("systemd/user"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|suffix| suffix == "service"))
        .expect("one installed unit");
    let descriptor = std::fs::read_to_string(unit).unwrap();
    assert!(descriptor.contains("--concurrency\" \"4\""), "{descriptor}");
    assert!(!descriptor.contains("worker-token"), "{descriptor}");
    let log = std::fs::read_to_string(&log).unwrap();
    assert_eq!(log.matches("--user enable").count(), 2, "{log}");
}

#[cfg(unix)]
#[test]
fn ca_wo_42_worker_service_status_is_absent_before_any_install() {
    let root = tempfile::tempdir().unwrap();
    let home = private_dir(&root.path().join("home"));
    let workdir = private_dir(&root.path().join("work"));
    let bin = fake_service_manager(root.path(), &root.path().join("manager.log"));
    store_worker_credential(&home, OUTPOST_ID);

    service_cli(&home, &bin)
        .args([
            "status",
            "--outpost",
            OUTPOST_ID,
            "--workdir",
            &workdir.to_string_lossy(),
        ])
        .assert()
        .success()
        .stdout("Worker service is not installed\n");
}

#[cfg(unix)]
#[test]
fn ca_bg_07_the_shim_escalates_to_sigkill_when_the_child_ignores_term() {
    let task_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        task_dir.path().join("cmd.sh"),
        "trap '' TERM\nsleep 300 &\nwait\n",
    )
    .unwrap();

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("cloudthinker"))
        .args(["worker", "bg-shim", "--task-dir"])
        .arg(task_dir.path())
        .args(["--timeout-secs", "1"])
        .stdin(std::process::Stdio::from(
            std::fs::File::open(task_dir.path()).unwrap(),
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let exit_file = task_dir.path().join("exit");
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    while !exit_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    let recorded = std::fs::read_to_string(&exit_file).expect("the shim records an exit code");
    assert_eq!(recorded, "137");

    let _ = child.wait();
    assert!(
        !std::path::Path::new(&format!("/proc/{}", child.id())).exists(),
        "the supervisor process group survived the escalation"
    );
}
