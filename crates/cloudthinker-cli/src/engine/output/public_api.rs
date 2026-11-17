use std::io::Write;

use cloudthinker_client::JsonDocument;
use cloudthinker_client::spec::OperationTable;

use super::{emit_json_to, terminal_text, write_line};

pub fn emit_public_operations(table: &OperationTable, json: bool) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if json {
        return emit_json_to(&mut out, table);
    }
    render_operations(&mut out, table)
}

fn render_operations(out: &mut impl Write, table: &OperationTable) -> Result<(), String> {
    write_line(out, "COMMAND\tOPERATION\tMETHOD\tSCOPE")?;
    for op in &table.operations {
        write_line(
            out,
            &format!(
                "cloudthinker {}\t{}\t{}\t{}",
                terminal_text(&op.cli.words().join(" ")),
                terminal_text(&op.id),
                op.method,
                terminal_text(op.scope.as_deref().unwrap_or("none")),
            ),
        )?;
    }
    Ok(())
}

pub fn emit_public_body(body: &JsonDocument, etag: Option<&str>) -> Result<(), String> {
    if let Some(etag) = etag {
        eprintln!("ETag: {}", terminal_text(etag));
    }
    let mut out = std::io::stdout().lock();
    emit_json_to(&mut out, body)
}
