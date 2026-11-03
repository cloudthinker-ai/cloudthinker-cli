use chrono::FixedOffset;
use cloudthinker_client::ConversationTrace;

use super::{emit_json, terminal_lines, terminal_text, write_line};

pub fn emit_trace(trace: &ConversationTrace, json: bool) -> Result<(), String> {
    if json {
        return emit_json(trace);
    }
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!("conversation: {}", trace.conversation_id),
    )?;
    write_line(&mut out, &format!("workspace: {}", trace.workspace_id))?;
    let offset = FixedOffset::east_opt(7 * 60 * 60)
        .ok_or_else(|| "could not format snapshot time".to_string())?;
    write_line(
        &mut out,
        &format!(
            "persisted snapshot: {} (may change while work continues)",
            trace.captured_at.with_timezone(&offset).to_rfc3339()
        ),
    )?;
    write_line(
        &mut out,
        &format!(
            "returned: {} messages, {} tool calls, {} recorded message failures; {} source messages excluded",
            trace.summary.messages,
            trace.summary.tool_calls,
            trace.summary.failed_messages,
            trace.truncation.messages_filtered
        ),
    )?;
    write_line(
        &mut out,
        &format!(
            "known usage for returned messages: {} tokens, {} credits; {} messages without usage, {} with unknown tokens",
            trace
                .summary
                .tokens
                .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
            trace
                .summary
                .credits
                .map_or_else(|| "unknown".to_string(), |value| format!("{value:.4}")),
            trace.summary.messages_without_usage,
            trace.summary.messages_with_unknown_tokens
        ),
    )?;
    if trace.messages.is_empty() {
        write_line(&mut out, "No visible persisted messages in this window.")?;
    }
    for message in &trace.messages {
        write_line(
            &mut out,
            &format!(
                "\n{} {} {} finish={}",
                message.created_at.with_timezone(&offset).to_rfc3339(),
                terminal_text(&message.role),
                message.id,
                message
                    .finish_reason
                    .as_deref()
                    .map_or_else(|| "unrecorded".to_string(), terminal_text)
            ),
        )?;
        write_line(&mut out, &terminal_lines(&message.text))?;
        write_line(
            &mut out,
            &format!(
                "  usage: {} tokens, {} credits ({} records, {} token records)",
                message
                    .usage
                    .tokens
                    .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                message
                    .usage
                    .credits
                    .map_or_else(|| "unknown".to_string(), |value| format!("{value:.4}")),
                message.usage.records,
                message.usage.token_records
            ),
        )?;
        for tool in &message.tools {
            write_line(
                &mut out,
                &format!(
                    "  tool: {} id={} call={} completed={} runtime={} approval={}",
                    terminal_text(&tool.name),
                    tool.id,
                    tool.tool_call_id
                        .as_deref()
                        .map_or_else(|| "unrecorded".to_string(), terminal_text),
                    tool.completed,
                    tool.runtime_seconds
                        .map_or_else(|| "unknown".to_string(), |value| format!("{value:.3}s")),
                    tool.approval_status
                        .as_deref()
                        .map_or_else(|| "unrecorded".to_string(), terminal_text)
                ),
            )?;
        }
    }
    write_line(&mut out, "Tool completion does not establish success.")?;
    if trace.truncation.truncated {
        write_line(
            &mut out,
            &format!(
                "Truncated: older messages={}, messages omitted for bytes={}, tools omitted={}, text fields clipped={}. Open the conversation in the browser for more.",
                trace.truncation.has_older_messages,
                trace.truncation.messages_omitted_for_bytes,
                trace.truncation.tools_omitted,
                trace.truncation.text_fields_truncated
            ),
        )?;
    }
    Ok(())
}
