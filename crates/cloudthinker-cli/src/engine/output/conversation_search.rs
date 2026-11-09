use chrono::FixedOffset;
use cloudthinker_client::{
    ConversationSearch, ConversationSearchMatchMode, ConversationSearchStatus,
};

use super::{emit_json, terminal_text, warn, write_line};

pub fn emit_conversation_search(search: &ConversationSearch, json: bool) -> Result<(), String> {
    for warning in &search.warnings {
        warn(warning);
    }
    if search.coverage.status == ConversationSearchStatus::Partial {
        warn(
            "search is partial: some authorized conversations were not searched; missing results do not prove absence",
        );
    }
    if json {
        return emit_json(search);
    }
    let offset = FixedOffset::east_opt(7 * 60 * 60)
        .ok_or_else(|| "could not format search time".to_string())?;
    let mut out = std::io::stdout().lock();
    write_line(
        &mut out,
        &format!(
            "Search \"{}\" in workspace {} ({}, case-insensitive)",
            terminal_text(&search.query),
            search.workspace_id,
            mode_label(search)
        ),
    )?;
    write_line(&mut out, &status_line(search))?;
    for (index, hit) in search.hits.iter().enumerate() {
        let title = hit
            .title
            .as_deref()
            .map_or_else(|| "(title unavailable)".to_string(), terminal_text);
        write_line(&mut out, &format!("\n{}. {title}", index + 1))?;
        let updated = hit.updated_at.map_or_else(
            || "unknown".to_string(),
            |value| value.with_timezone(&offset).to_rfc3339(),
        );
        write_line(
            &mut out,
            &format!(
                "   {}  {}  updated {updated}",
                hit.conversation_id,
                hit.conversation_type
                    .as_deref()
                    .map_or_else(|| "unknown".to_string(), terminal_text),
            ),
        )?;
        let source = match (
            hit.evidence.role.as_deref(),
            hit.evidence.tool_name.as_deref(),
        ) {
            (Some(role), _) => format!("{} message", terminal_text(role)),
            (None, Some(tool)) => format!("tool {}", terminal_text(tool)),
            (None, None) => terminal_text(&hit.evidence.record_type),
        };
        let score = hit
            .evidence
            .score
            .map_or_else(String::new, |score| format!(", score {score:.1}"));
        write_line(
            &mut out,
            &format!(
                "   {source} {} (line {}{score}): {}",
                terminal_text(&hit.evidence.field),
                hit.evidence.line,
                terminal_text(&hit.evidence.snippet)
            ),
        )?;
        write_line(&mut out, &format!("   {}", hit.web_url))?;
    }
    if let Some(cursor) = &search.coverage.next_cursor {
        let expires = search.coverage.cursor_expires_at.map_or_else(
            || "soon".to_string(),
            |value| value.with_timezone(&offset).to_rfc3339(),
        );
        write_line(
            &mut out,
            &format!(
                "\nNext page: run the same search with --cursor {} (expires {expires})",
                terminal_text(cursor)
            ),
        )?;
    }
    if let Some(hint) = &search.coverage.hint {
        write_line(&mut out, &terminal_text(hint))?;
    }
    Ok(())
}

fn mode_label(search: &ConversationSearch) -> String {
    match search.match_mode {
        ConversationSearchMatchMode::Literal => "literal text".to_string(),
        ConversationSearchMatchMode::Regex => "regular expression".to_string(),
        ConversationSearchMatchMode::Fuzzy => format!(
            "fuzzy, score at least {}",
            search
                .min_score
                .map_or_else(|| "unknown".to_string(), |score| score.to_string())
        ),
    }
}

fn status_line(search: &ConversationSearch) -> String {
    let count = search.hits.len();
    let noun = if count == 1 {
        "conversation"
    } else {
        "conversations"
    };
    match search.coverage.status {
        ConversationSearchStatus::Complete if count == 0 => {
            "No matches. Every authorized conversation in scope was searched.".to_string()
        }
        ConversationSearchStatus::Complete => format!(
            "{count} matching {noun}. Complete: every authorized conversation in scope was searched."
        ),
        ConversationSearchStatus::LimitReached => {
            format!("{count} matching {noun}. Limit reached: more conversations may match.")
        }
        ConversationSearchStatus::Partial => {
            let reasons = if search.coverage.reasons.is_empty() {
                "unspecified".to_string()
            } else {
                search
                    .coverage
                    .reasons
                    .iter()
                    .map(String::as_str)
                    .map(terminal_text)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if count == 0 {
                format!(
                    "PARTIAL: no matches in the part searched ({} sources), which does not prove there are none. Reasons: {reasons}.",
                    search.coverage.scanned_sources
                )
            } else {
                format!(
                    "{count} matching {noun}. PARTIAL: {} sources searched; other conversations may match. Reasons: {reasons}.",
                    search.coverage.scanned_sources
                )
            }
        }
    }
}
