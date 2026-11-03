use std::io::{BufRead, IsTerminal, Write};
use std::time::{Duration, Instant};

use cloudthinker_client::{
    ConnectionDetail, ConnectionDisconnectPhase, ConnectionKind, ConnectionMatch, ConnectionStatus,
    ConnectionTest, ConnectionView, CtClient, CtError, explicit_url, validate_connect_prefix,
};
use uuid::Uuid;

use crate::engine::exit::{self, ExitCode};
use crate::engine::login_mode::{self, LoginEnvironment, LoginFlow};
use crate::engine::output::{self, ConnectLink};
use crate::engine::watch::{Poll, WatchConfig, watch};

use super::build_client;
use super::chat::{Interrupted, listen_for_interrupt};

pub struct Wait {
    pub enabled: bool,
    pub timeout_secs: u64,
}

pub async fn run_list(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.list_connections().await {
        Ok(connections) => {
            exit::finish_output(output::emit_connections(&connections.installed, json))
        }
        Err(err) => exit::report(&err),
    }
}

pub async fn run_show(
    base_url: &str,
    workspace: Option<&str>,
    target: &str,
    json: bool,
) -> ExitCode {
    let (client, connection) = match resolve(base_url, workspace, target).await {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let detail = match connection.kind {
        ConnectionKind::Builtin => match client.builtin_connection(connection.id).await {
            Ok(detail) => detail,
            Err(CtError::Api { status: 404, .. }) => return no_match(target),
            Err(err) => return exit::report(&err),
        },
        ConnectionKind::Mcp => ConnectionDetail {
            connection,
            locations: Vec::new(),
        },
    };
    exit::finish_output(output::emit_connection(&detail, json))
}

pub async fn run_status(
    base_url: &str,
    workspace: Option<&str>,
    target: &str,
    wait: Wait,
    json: bool,
) -> ExitCode {
    let (client, connection) = match resolve(base_url, workspace, target).await {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    let test = if wait.enabled {
        wait_for_test(
            &client,
            &connection,
            Duration::from_secs(wait.timeout_secs),
            &mut listen_for_interrupt(),
        )
        .await
    } else {
        client
            .connection_test(connection.id, connection.kind)
            .await
            .map_err(Waited::Failed)
    };
    finish_test(&connection, test, target, json)
}

pub async fn run_retry(
    base_url: &str,
    workspace: Option<&str>,
    target: &str,
    wait: Wait,
    json: bool,
) -> ExitCode {
    let (client, connection) = match resolve(base_url, workspace, target).await {
        Ok(resolved) => resolved,
        Err(code) => return code,
    };
    if connection.kind == ConnectionKind::Mcp {
        output::eprintln_error(&format!(
            "`retry` re-tests built-in connections only, and `{}` is an MCP connection. Check its last test with: cloudthinker connections status {}",
            output::terminal_text(&connection.name),
            connection.id
        ));
        return ExitCode::Usage;
    }
    if !connection.can_retry {
        output::eprintln_error(&format!(
            "`{}` signs in through the browser, so a retry would reuse an expired sign-in. Reconnect it with: cloudthinker connections connect-url {}",
            output::terminal_text(&connection.name),
            output::terminal_text(&connection.prefix)
        ));
        return ExitCode::JobFailed;
    }
    let mut interrupted = listen_for_interrupt();
    let queued = match client.retry_connection(connection.id).await {
        Ok(test) => test,
        Err(CtError::Api { status: 409, .. }) => return retry_conflict(&connection),
        Err(CtError::Api { status: 404, .. }) => return no_match(target),
        Err(err) => return exit::report(&err),
    };
    if !wait.enabled {
        output::progress(&format!(
            "Test queued. Follow it with: {}",
            resume_command(connection.id)
        ));
        return finish_test(&connection, Ok(queued), target, json);
    }
    let test = wait_for_test(
        &client,
        &connection,
        Duration::from_secs(wait.timeout_secs),
        &mut interrupted,
    )
    .await;
    finish_test(&connection, test, target, json)
}

pub async fn run_disconnect(
    base_url: &str,
    workspace: Option<&str>,
    target: &str,
    yes: bool,
    json: bool,
) -> ExitCode {
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if consent_gate(yes, interactive) == Consent::Refuse {
        return refuse_without_yes(base_url, workspace, target);
    }
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let mut input = std::io::BufReader::new(std::io::stdin());
    disconnect_with(&client, target, yes, json, interactive, &mut input).await
}

async fn disconnect_with(
    client: &CtClient,
    target: &str,
    yes: bool,
    json: bool,
    interactive: bool,
    input: &mut impl BufRead,
) -> ExitCode {
    let gate = consent_gate(yes, interactive);
    if gate == Consent::Refuse {
        return ExitCode::Usage;
    }
    let connection = match find(client, target).await {
        Ok(connection) => connection,
        Err(code) => return code,
    };
    if gate == Consent::Ask {
        let prompt = confirm_prompt(
            &connection.name,
            connection.kind,
            &connection.prefix,
            connection.id,
        );
        if !ask(&prompt, input) {
            output::progress("Cancelled. Nothing was disconnected.");
            return ExitCode::JobFailed;
        }
    }
    let accepted = match client
        .disconnect_connection(connection.id, connection.kind)
        .await
    {
        Ok(accepted) => accepted,
        Err(CtError::Api { status: 404, .. }) => return no_match(target),
        Err(err) => return exit::report(&err),
    };
    let code = if accepted.disconnect_phase == ConnectionDisconnectPhase::Failed {
        output::eprintln_error(&format!(
            "disconnecting `{}` failed: {}",
            output::terminal_text(&accepted.connection.name),
            accepted
                .disconnect_error
                .as_deref()
                .unwrap_or("no reason was reported")
        ));
        ExitCode::JobFailed
    } else {
        output::progress(
            "Disconnect started. CloudThinker finishes it in the background, and the connection leaves `cloudthinker connections list` when it is done.",
        );
        ExitCode::Ok
    };
    match output::emit_connection_disconnect(&accepted, json) {
        Ok(()) => code,
        Err(error) => exit::finish_output(Err(error)),
    }
}

fn refuse_without_yes(base_url: &str, workspace: Option<&str>, target: &str) -> ExitCode {
    output::eprintln_error(&format!(
        "`connections disconnect` deletes a connection, and it cannot ask you to confirm without a terminal. Run it again with --yes: {}",
        output::terminal_text(&disconnect_command(
            explicit_url(base_url).as_deref(),
            workspace,
            target
        ))
    ));
    ExitCode::Usage
}

fn disconnect_command(url: Option<&str>, workspace: Option<&str>, target: &str) -> String {
    let mut words = vec!["cloudthinker".to_string()];
    if let Some(url) = url {
        words.extend(["--url".to_string(), output::shell_word(url)]);
    }
    if let Some(workspace) = workspace {
        words.extend(["--workspace".to_string(), output::shell_word(workspace)]);
    }
    words.extend([
        "connections".to_string(),
        "disconnect".to_string(),
        output::shell_word(target),
        "--yes".to_string(),
    ]);
    words.join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consent {
    Given,
    Ask,
    Refuse,
}

fn consent_gate(yes: bool, interactive: bool) -> Consent {
    match (yes, interactive) {
        (true, _) => Consent::Given,
        (false, true) => Consent::Ask,
        (false, false) => Consent::Refuse,
    }
}

fn confirm_prompt(name: &str, kind: ConnectionKind, prefix: &str, id: Uuid) -> String {
    format!(
        "Disconnect `{}` ({} {}, {id})? This deletes the connection and its saved credentials from the workspace, and agents can no longer use it. [y/N] ",
        output::terminal_text(name),
        kind.label(),
        output::terminal_text(prefix)
    )
}

fn ask(prompt: &str, input: &mut impl BufRead) -> bool {
    eprint!("{prompt}");
    if let Err(error) = std::io::stderr().flush() {
        output::eprintln_error(&format!(
            "could not show the confirmation question: {error}; nothing was disconnected"
        ));
        return false;
    }
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(_) => accepts(&line),
        Err(error) => {
            output::eprintln_error(&format!(
                "could not read your answer: {error}; nothing was disconnected"
            ));
            false
        }
    }
}

fn accepts(line: &str) -> bool {
    matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES")
}

pub async fn run_connect_url(
    base_url: &str,
    workspace: Option<&str>,
    prefix: &str,
    wait: Wait,
    json: bool,
) -> ExitCode {
    if let Err(err) = validate_connect_prefix(prefix) {
        return exit::report(&err);
    }
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let (connections, identity) = match tokio::try_join!(client.list_connections(), client.whoami())
    {
        Ok(found) => found,
        Err(err) => return exit::report(&err),
    };
    if !connections.offers_prefix(prefix) {
        output::eprintln_error(&format!(
            "no connection type is called `{prefix}`; `cloudthinker connections list` shows the prefixes in use, and the Connectors page in the web app lists them all"
        ));
        return ExitCode::Usage;
    }
    let url = match client.connect_url(prefix, identity.workspace_id) {
        Ok(url) => url,
        Err(err) => return exit::report(&err),
    };
    let opened = open_in_browser(&url);
    let mut link = ConnectLink {
        url,
        prefix: prefix.to_string(),
        workspace_id: identity.workspace_id,
        opened,
        connection: None,
        test: None,
    };
    output::progress(if opened {
        "Opened the connect page in your browser. If it did not open, use this URL:"
    } else {
        "Open this URL in your browser to connect:"
    });
    if !wait.enabled {
        return exit::finish_output(output::emit_connect_link(&link, json));
    }
    if json {
        output::progress(&link.url);
    } else if let Err(error) = output::emit_connect_link(&link, false) {
        return exit::finish_output(Err(error));
    }
    let deadline = Instant::now() + Duration::from_secs(wait.timeout_secs);
    let mut interrupted = listen_for_interrupt();
    let known = connections.builtin_ids_with_prefix(prefix);
    let errored: Vec<Uuid> = connections
        .installed
        .iter()
        .filter(|row| known.contains(&row.id) && row.status == ConnectionStatus::Error)
        .map(|row| row.id)
        .collect();
    let created = match wait_for_new_connection(
        &client,
        prefix,
        &known,
        &errored,
        deadline,
        &mut interrupted,
    )
    .await
    {
        Ok(created) => created,
        Err(Waited::Failed(CtError::Timeout(_))) => {
            output::progress(&format!(
                "Timed out before a new {prefix} connection appeared. Check with: cloudthinker connections list"
            ));
            return finish_link(&link, json, ExitCode::Timeout);
        }
        Err(Waited::Failed(err)) => {
            let code = exit::report(&err);
            return finish_link(&link, json, code);
        }
        Err(Waited::Interrupted) => {
            output::progress("Stopped waiting. Check with: cloudthinker connections list");
            return finish_link(&link, json, ExitCode::Interrupted);
        }
    };
    output::progress(&format!(
        "Connection {} was {}; waiting for its test.",
        created.id,
        if known.contains(&created.id) {
            "reconnected"
        } else {
            "added"
        }
    ));
    let remaining = deadline.saturating_duration_since(Instant::now());
    let test = wait_for_test(&client, &created, remaining, &mut interrupted).await;
    if !json {
        return finish_test(&created, test, prefix, false);
    }
    let code = match test_outcome(&created, test, prefix) {
        Ok((test, code)) => {
            link.test = Some(test);
            code
        }
        Err(code) => code,
    };
    link.connection = Some(created);
    finish_link(&link, true, code)
}

fn finish_link(link: &ConnectLink, json: bool, code: ExitCode) -> ExitCode {
    if !json {
        return code;
    }
    match output::emit_connect_link(link, true) {
        Ok(()) => code,
        Err(error) => exit::finish_output(Err(error)),
    }
}

enum Waited {
    Failed(CtError),
    Interrupted,
}

async fn resolve(
    base_url: &str,
    workspace: Option<&str>,
    target: &str,
) -> Result<(CtClient, ConnectionView), ExitCode> {
    let client = build_client(base_url, workspace).map_err(|err| exit::report(&err))?;
    let connection = find(&client, target).await?;
    Ok((client, connection))
}

async fn find(client: &CtClient, target: &str) -> Result<ConnectionView, ExitCode> {
    let connections = client
        .list_connections()
        .await
        .map_err(|err| exit::report(&err))?;
    match connections.resolve(target) {
        ConnectionMatch::One(connection) => Ok(*connection),
        ConnectionMatch::None => Err(no_match(target)),
        ConnectionMatch::Many(candidates) => {
            output::eprintln_error(&format!(
                "`{}` matches {} connections; pass one connection id:",
                output::terminal_text(target),
                candidates.len()
            ));
            for candidate in &candidates {
                output::progress(&format!(
                    "  {}  {}  {}",
                    candidate.id,
                    candidate.kind.label(),
                    output::terminal_text(&candidate.name)
                ));
            }
            Err(ExitCode::Usage)
        }
    }
}

fn no_match(target: &str) -> ExitCode {
    output::eprintln_error(&format!(
        "no connection matches `{}` in this workspace; `cloudthinker connections list` shows them, and `--workspace <id|name>` looks in another workspace",
        output::terminal_text(target)
    ));
    ExitCode::JobFailed
}

fn retry_conflict(connection: &ConnectionView) -> ExitCode {
    let name = output::terminal_text(&connection.name);
    if connection.status == ConnectionStatus::Disconnecting {
        output::eprintln_error(&format!(
            "`{name}` is being disconnected, so it cannot be retried"
        ));
    } else {
        output::eprintln_error(&format!(
            "`{name}` is already testing. Follow it with: {}",
            resume_command(connection.id)
        ));
    }
    ExitCode::JobFailed
}

fn resume_command(id: Uuid) -> String {
    format!("cloudthinker connections status {id} --wait")
}

fn finish_test(
    connection: &ConnectionView,
    test: Result<ConnectionTest, Waited>,
    target: &str,
    json: bool,
) -> ExitCode {
    match test_outcome(connection, test, target) {
        Ok((test, code)) => match output::emit_connection_test(connection, &test, json) {
            Ok(()) => code,
            Err(error) => exit::finish_output(Err(error)),
        },
        Err(code) => code,
    }
}

fn test_outcome(
    connection: &ConnectionView,
    test: Result<ConnectionTest, Waited>,
    target: &str,
) -> Result<(ConnectionTest, ExitCode), ExitCode> {
    let resume = resume_command(connection.id);
    match test {
        Ok(test) if test.phase.is_failure() => {
            output::eprintln_error(&format!(
                "connection test {}: {}",
                output::terminal_text(test.phase.label()),
                test.last_error
                    .as_deref()
                    .unwrap_or("no reason was reported")
            ));
            Ok((test, ExitCode::JobFailed))
        }
        Ok(test) => Ok((test, ExitCode::Ok)),
        Err(Waited::Failed(CtError::Api { status: 404, .. })) => Err(no_match(target)),
        Err(Waited::Failed(CtError::Timeout(_))) => {
            output::progress(&format!(
                "Timed out. The test keeps running; resume with: {resume}"
            ));
            Err(ExitCode::Timeout)
        }
        Err(Waited::Failed(err)) => Err(exit::report(&err)),
        Err(Waited::Interrupted) => {
            output::progress(&format!(
                "Stopped waiting. The test keeps running; resume with: {resume}"
            ));
            Err(ExitCode::Interrupted)
        }
    }
}

async fn wait_for_test(
    client: &CtClient,
    connection: &ConnectionView,
    timeout: Duration,
    interrupted: &mut Interrupted,
) -> Result<ConnectionTest, Waited> {
    let status = output::live_status(&format!(
        "Testing {}",
        output::terminal_text(&connection.name)
    ));
    let cfg = WatchConfig::for_run(timeout);
    tokio::select! {
        outcome = watch(
            async || {
                let test = client.connection_test(connection.id, connection.kind).await?;
                status.set_message(format!(
                    "Testing {}: {}",
                    output::terminal_text(&connection.name),
                    output::terminal_text(test.phase.label())
                ));
                if test.terminal {
                    Ok(Poll::Terminal(test))
                } else {
                    Ok(Poll::Pending)
                }
            },
            &cfg,
        ) => outcome.map_err(Waited::Failed),
        () = interrupted => Err(Waited::Interrupted),
    }
}

async fn wait_for_new_connection(
    client: &CtClient,
    prefix: &str,
    known: &[Uuid],
    errored: &[Uuid],
    deadline: Instant,
    interrupted: &mut Interrupted,
) -> Result<ConnectionView, Waited> {
    let _status = output::live_status(&format!("Waiting for a new {prefix} connection"));
    let cfg = WatchConfig::for_run(deadline.saturating_duration_since(Instant::now()));
    tokio::select! {
        outcome = watch(
            async || {
                let connections = client.list_connections().await?;
                let created = connections.installed.into_iter().find(|row| {
                    row.kind == ConnectionKind::Builtin
                        && row.prefix == prefix
                        && (!known.contains(&row.id)
                            || (errored.contains(&row.id) && row.status != ConnectionStatus::Error))
                });
                Ok(created.map_or(Poll::Pending, Poll::Terminal))
            },
            &cfg,
        ) => outcome.map_err(Waited::Failed),
        () = interrupted => Err(Waited::Interrupted),
    }
}

fn open_in_browser(url: &str) -> bool {
    if login_mode::choose(&LoginEnvironment::from_process(false)) != LoginFlow::Browser {
        return false;
    }
    open::that(url).is_ok()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use cloudthinker_client::{ConnectionKind, CtClient, StoredToken, TokenStore};
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        Consent, accepts, ask, confirm_prompt, consent_gate, disconnect_command, disconnect_with,
    };
    use crate::engine::exit::ExitCode;

    const AWS_ID: &str = "aaaaaaaa-0000-4000-8000-000000000001";

    struct AccessToken;

    impl TokenStore for AccessToken {
        fn load(&self) -> cloudthinker_client::CtResult<Option<StoredToken>> {
            Ok(Some(StoredToken {
                access_token: "access".into(),
                refresh_token: None,
                expires_at: None,
                workspace_id: None,
                workspace_name: None,
            }))
        }

        fn save(&self, _token: &StoredToken) -> cloudthinker_client::CtResult<()> {
            Ok(())
        }

        fn clear(&self) -> cloudthinker_client::CtResult<()> {
            Ok(())
        }

        fn refresh_enabled(&self) -> bool {
            false
        }
    }

    async fn workspace_with_aws() -> MockServer {
        let server = MockServer::start().await;
        let builtin = format!(
            r#"{{"data":[{{"id":"{AWS_ID}","name":"aws account","prefix":"aws","alias":"aws-prod","aliases":[],"builtin_connection_type":"cloud","is_connected":true,"is_sandbox_supported":true,"connection_env_type":"production","status":"connected","status_message":"","auth_type":"STATIC","can_retry":true,"updated_at":"2026-10-30T00:00:00Z"}}],"connected_count":1}}"#
        );
        let accepted = format!(
            r#"{{"connection":{{"id":"{AWS_ID}","name":"aws account","prefix":"aws","alias":"aws-prod","aliases":[],"builtin_connection_type":"cloud","is_connected":true,"is_sandbox_supported":true,"connection_env_type":"production","status":"disconnecting","status_message":"Disconnect queued","auth_type":"STATIC","can_retry":true,"updated_at":"2026-10-30T00:00:00Z"}},"disconnect_status":{{"phase":"queued"}}}}"#
        );
        Mock::given(method("GET"))
            .and(path("/api/v1/builtin-connection"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(builtin, "application/json"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/mcp-connection"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"data":[],"count":0,"connected_count":0}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("/api/v1/builtin-connection/{AWS_ID}")))
            .respond_with(ResponseTemplate::new(202).set_body_raw(accepted, "application/json"))
            .mount(&server)
            .await;
        server
    }

    async fn deletes(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method.as_str() == "DELETE")
            .count()
    }

    #[tokio::test]
    async fn declining_the_question_sends_no_delete() {
        let server = workspace_with_aws().await;
        let client = CtClient::new(server.uri(), Arc::new(AccessToken)).unwrap();

        for answer in ["n\n", "\n", ""] {
            let code = disconnect_with(
                &client,
                "aws-prod",
                false,
                true,
                true,
                &mut Cursor::new(answer),
            )
            .await;
            assert_eq!(code, ExitCode::JobFailed, "{answer:?}");
        }
        assert_eq!(deletes(&server).await, 0);

        let code = disconnect_with(
            &client,
            "aws-prod",
            false,
            true,
            true,
            &mut Cursor::new("y\n"),
        )
        .await;
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(deletes(&server).await, 1);
    }

    #[test]
    fn refusal_hint_keeps_the_host_and_workspace() {
        assert_eq!(
            disconnect_command(None, None, "aws-prod"),
            "cloudthinker connections disconnect aws-prod --yes"
        );
        assert_eq!(
            disconnect_command(
                Some("https://dev.cloudthinker.io"),
                Some("Team Ops"),
                "it's mine; rm -rf /"
            ),
            r"cloudthinker --url https://dev.cloudthinker.io --workspace 'Team Ops' connections disconnect 'it'\''s mine; rm -rf /' --yes"
        );
    }

    #[test]
    fn disconnect_asks_on_a_terminal_and_refuses_without_one() {
        assert_eq!(consent_gate(true, true), Consent::Given);
        assert_eq!(consent_gate(true, false), Consent::Given);
        assert_eq!(consent_gate(false, true), Consent::Ask);
        assert_eq!(consent_gate(false, false), Consent::Refuse);
    }

    #[test]
    fn disconnect_prompt_names_the_connection() {
        let id = Uuid::from_u128(7);
        let prompt = confirm_prompt("AWS \u{1b}[31mprod", ConnectionKind::Builtin, "aws", id);
        assert!(
            prompt.starts_with(&format!("Disconnect `AWS [31mprod` (builtin aws, {id})? ")),
            "{prompt}"
        );
        assert!(prompt.ends_with("[y/N] "), "{prompt}");
    }

    #[test]
    fn only_an_explicit_yes_confirms_a_disconnect() {
        for line in ["y\n", "Y", "yes\n", " Yes "] {
            assert!(accepts(line), "{line:?}");
        }
        for line in ["", "\n", "n", "no", "yep", "aws"] {
            assert!(!accepts(line), "{line:?}");
        }
        assert!(ask("", &mut Cursor::new("yes\n")));
        assert!(!ask("", &mut Cursor::new("\n")));
        assert!(!ask("", &mut Cursor::new("")));
    }
}
