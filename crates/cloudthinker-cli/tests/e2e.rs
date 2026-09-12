//! End-to-end binary tests driving `cloudthinker` against a canned HTTP server.
//!
//! `CLOUDTHINKER_TOKEN` supplies an access-only credential so no keyring or
//! login is involved, and the mock server returns terminal runs immediately so
//! the watch loop finishes in one poll.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// This integration target links the whole crate's dependency set but only needs
// a few; silence `unused_crate_dependencies` for the bin-only deps.
use axoupdater as _;
use clap as _;
use cloudthinker_client as _;
use open as _;
use owo_colors as _;
use rand as _;
use serde as _;
use supports_color as _;
use tokio as _;

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use assert_cmd::Command;

const RUN_ID: &str = "11111111-1111-4111-8111-111111111111";
const CONV_ID: &str = "22222222-2222-4222-8222-222222222222";

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
                        socket.set_nonblocking(false).ok();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .ok();
                        let mut data = Vec::new();
                        let mut buf = [0u8; 4096];
                        loop {
                            match socket.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => data.extend_from_slice(&buf[..n]),
                                Err(_) => break,
                            }
                        }
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

const WHOAMI_BODY: &str = r#"{"user_email":"duc@example.com","workspace_id":"11111111-1111-4111-8111-111111111111","workspace_name":"Production","organization_id":null}"#;

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
// "submitted" progress line goes to stderr.
#[test]
fn ca_cli_10_stdout_is_answer_only() {
    let api = MockApi::start(status_body("succeeded", "the final answer", ""));
    cli(&api.base_url)
        .args(["chat", "-p", "hello"])
        .assert()
        .success()
        .stdout("the final answer\n")
        .stderr(predicates::str::contains("Submitted"))
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

/// Release JSON for the GitHub API `releases/latest` endpoint, with the
/// installer asset pointing back at the mock server.
fn release_body(mock_base: &str, tag: &str) -> String {
    format!(
        r#"{{"tag_name":"{tag}","name":"{tag}","url":"{mock_base}/releases/{tag}","assets":[{{"name":"{UPDATE_APP}-installer.sh","url":"{mock_base}/installer.sh","browser_download_url":"{mock_base}/installer.sh"}}],"prerelease":false}}"#
    )
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
}

impl MockReleases {
    /// `installer_script` is served as the release's installer; give it a
    /// side effect (e.g. touch a marker) to prove it actually executed.
    fn start(tag: &str, installer_script: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let base_url = format!("http://{addr}");
        let release_body = release_body(&base_url, tag);

        let handle = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket.set_nonblocking(false).ok();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .ok();
                        let mut data = Vec::new();
                        let mut buf = [0u8; 4096];
                        loop {
                            match socket.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => data.extend_from_slice(&buf[..n]),
                                Err(_) => break,
                            }
                        }
                        let request = String::from_utf8_lossy(&data);
                        let first_line = request.lines().next().unwrap_or_default();
                        let (status, body, content_type) = if first_line.contains("/api/v3/repos/")
                        {
                            ("200 OK", release_body.clone(), "application/json")
                        } else if first_line.contains("/installer.sh") {
                            ("200 OK", installer_script.clone(), "text/plain")
                        } else {
                            ("404 Not Found", String::new(), "text/plain")
                        };
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
        });

        Self {
            base_url,
            stop,
            handle: Some(handle),
        }
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
        .env_remove("NO_COLOR");
    cmd
}

/// Fake installer: writes a marker file whose path arrives via env, so tests
/// can prove the updater downloaded AND executed the installer.
fn marker_installer_script() -> String {
    "#!/bin/sh\ntouch \"$CT_UPDATE_TEST_MARKER\"\n".to_string()
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
        .stdout("Updated cloudthinker from 0.1.0 to 0.2.0\n");
    assert!(
        std::path::Path::new(&marker).exists(),
        "installer must have run"
    );
    let _ = std::fs::remove_file(&marker);
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
fn agent_on_a_tty(api: &MockApi, releases: &MockReleases, receipt_dir: &std::path::Path) -> String {
    binary_on_a_tty(
        &assert_cmd::cargo::cargo_bin("cloudthinker"),
        &["agent"],
        b"n\n",
        api,
        releases,
        receipt_dir,
    )
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
// versions, and declining it still starts the session.
#[cfg(unix)]
#[test]
fn ca_up_9_a_newer_release_is_offered_and_declining_still_starts_the_agent() {
    let api = MockApi::start(WHOAMI_BODY.into());
    let tag = newer_tag();
    let releases = MockReleases::start(&tag, marker_installer_script());
    let receipt_dir = write_receipt("up6", RUNNING_VERSION, &real_install_prefix());

    let output = agent_on_a_tty(&api, &releases, &receipt_dir);

    assert!(
        output.contains(&format!(
            "cloudthinker {} is available (you have {RUNNING_VERSION})",
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

/// A private copy of the built binary that a fake installer may replace, plus
/// the receipt that claims it. The real install location must stay untouched.
#[cfg(unix)]
fn installed_copy(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("ct-installed-copy-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let binary = dir.join("cloudthinker");
    std::fs::copy(assert_cmd::cargo::cargo_bin("cloudthinker"), &binary).unwrap();
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

    let output = binary_on_a_tty(
        &binary,
        &["agent", "--", "--resume"],
        b"y\n",
        &api,
        &releases,
        &receipt_dir,
    );

    assert!(
        output.contains(&format!(
            "Updated cloudthinker from {RUNNING_VERSION} to {}",
            tag.trim_start_matches('v')
        )),
        "expected the install, got:\n{output}"
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

    let output = agent_on_a_tty(&api, &releases, &receipt_dir);

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

#[test]
fn ca_cli_skill_exports_bundled_modules_offline() {
    for (topic, file) in [
        ("index", "SKILL.md"),
        ("auth", "auth.md"),
        ("chat", "chat.md"),
        ("review", "review.md"),
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
