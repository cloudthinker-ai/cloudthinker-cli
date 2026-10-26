use cloudthinker_client::{ConnectionDetail, ConnectionTest, ConnectionView};
use serde::Serialize;
use uuid::Uuid;

use super::{emit_json, progress, terminal_text, write_line};

#[derive(Debug, Serialize)]
pub struct ConnectLink {
    pub url: String,
    pub prefix: String,
    pub workspace_id: Uuid,
    pub opened: bool,
    pub connection: Option<ConnectionView>,
    pub test: Option<ConnectionTest>,
}

pub fn emit_connections(connections: &[ConnectionView], json: bool) -> Result<(), String> {
    if json {
        return emit_json(&connections);
    }
    if connections.is_empty() {
        progress("No connections.");
        progress("Connect one in the browser with: cloudthinker connections connect-url <prefix>");
        return Ok(());
    }
    let mut out = std::io::stdout().lock();
    write_line(&mut out, "CONNECTION ID\tTYPE\tPREFIX\tALIAS\tSTATUS\tNAME")?;
    for connection in connections {
        write_line(
            &mut out,
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                connection.id,
                connection.kind.label(),
                terminal_text(&connection.prefix),
                connection
                    .alias
                    .as_deref()
                    .map_or_else(|| "-".to_string(), terminal_text),
                terminal_text(connection.status.label()),
                terminal_text(&connection.name)
            ),
        )?;
    }
    Ok(())
}

pub fn emit_connection(detail: &ConnectionDetail, json: bool) -> Result<(), String> {
    if json {
        return emit_json(detail);
    }
    let mut lines = connection_lines(&detail.connection);
    lines.extend(detail.locations.iter().map(|location| {
        format!(
            "{}: {}",
            terminal_text(&location.key),
            terminal_text(&location.value)
        )
    }));
    let mut out = std::io::stdout().lock();
    for line in lines {
        write_line(&mut out, &line)?;
    }
    Ok(())
}

pub fn emit_connection_test(
    connection: &ConnectionView,
    test: &ConnectionTest,
    json: bool,
) -> Result<(), String> {
    if json {
        return emit_json(test);
    }
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &format!("connection: {}", connection.id))?;
    write_line(
        &mut out,
        &format!("name:       {}", terminal_text(&connection.name)),
    )?;
    write_line(
        &mut out,
        &format!("test:       {}", terminal_text(test.phase.label())),
    )?;
    if let Some(elapsed_ms) = test.elapsed_ms {
        write_line(
            &mut out,
            &format!("elapsed:    {:.1}s", elapsed_ms as f64 / 1000.0),
        )?;
    }
    Ok(())
}

pub fn emit_connect_link(link: &ConnectLink, json: bool) -> Result<(), String> {
    if json {
        return emit_json(link);
    }
    let mut out = std::io::stdout().lock();
    write_line(&mut out, &link.url)
}

fn connection_lines(connection: &ConnectionView) -> Vec<String> {
    let mut lines = vec![
        format!("connection: {}", connection.id),
        format!("name:       {}", terminal_text(&connection.name)),
        format!("type:       {}", connection.kind.label()),
        format!("prefix:     {}", terminal_text(&connection.prefix)),
    ];
    let optional = [
        ("alias:     ", connection.alias.as_deref()),
        ("transport: ", connection.transport.as_deref()),
        ("auth:      ", connection.auth.as_deref()),
    ];
    lines.extend(optional.into_iter().filter_map(|(label, value)| {
        value.map(|value| format!("{label} {}", terminal_text(value)))
    }));
    lines.push(format!(
        "status:     {}",
        terminal_text(connection.status.label())
    ));
    if let Some(message) = connection.status_message.as_deref() {
        lines.push(format!("message:    {}", terminal_text(message)));
    }
    if let Some(updated_at) = connection.updated_at {
        lines.push(format!("updated:    {}", updated_at.to_rfc3339()));
    }
    if let Some(url) = connection.documentation_url.as_deref() {
        lines.push(format!("docs:       {}", terminal_text(url)));
    }
    lines
}
