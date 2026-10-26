use std::time::{Duration, Instant};

use cloudthinker_client::{
    ConnectionDetail, ConnectionKind, ConnectionMatch, ConnectionStatus, ConnectionTest,
    ConnectionView, CtClient, CtError, validate_connect_prefix,
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
    let connections = client
        .list_connections()
        .await
        .map_err(|err| exit::report(&err))?;
    match connections.resolve(target) {
        ConnectionMatch::One(connection) => Ok((client, *connection)),
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
