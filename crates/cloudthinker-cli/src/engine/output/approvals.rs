use std::io::Write;

use cloudthinker_client::{ApprovalPage, ApprovalState, ApprovalView};

use super::{emit_json_to, progress, shell_word, terminal_text, write_line};

pub fn emit_approval_page(page: &ApprovalPage, json: bool) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if json {
        return emit_json_to(&mut out, page);
    }
    if page.items.is_empty() {
        progress("No approval requests wait for you.");
    } else {
        render_page(&mut out, page)?;
    }
    if let Some(cursor) = &page.next_cursor {
        progress(&format!(
            "More requests: run the same command with --cursor {}",
            shell_word(cursor)
        ));
    }
    Ok(())
}

pub fn emit_approval_watch(view: &ApprovalView, json: bool, first: bool) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if json {
        let line = serde_json::to_string(view).map_err(|error| error.to_string())?;
        return write_line(&mut out, &line);
    }
    if first {
        render_request(&mut out, view)
    } else {
        write_line(&mut out, &decision_line(view))
    }
}

fn render_page(out: &mut impl Write, page: &ApprovalPage) -> Result<(), String> {
    write_line(
        out,
        "REQUEST ID\tKIND\tASKED AT\tEXPIRES AT\tREQUESTER\tTITLE",
    )?;
    for view in &page.items {
        write_line(
            out,
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                view.request_id,
                view.kind.label(),
                view.asked_at.to_rfc3339(),
                view.expires_at.to_rfc3339(),
                terminal_text(view.requester.name.as_deref().unwrap_or("unknown")),
                terminal_text(&view.title)
            ),
        )?;
    }
    Ok(())
}

fn render_request(out: &mut impl Write, view: &ApprovalView) -> Result<(), String> {
    write_line(out, &format!("request:   {}", view.request_id))?;
    write_line(out, &format!("title:     {}", terminal_text(&view.title)))?;
    write_line(
        out,
        &format!("action:    {}", terminal_text(&view.object.label)),
    )?;
    write_line(out, &format!("kind:      {}", view.kind.label()))?;
    write_line(
        out,
        &format!(
            "requester: {}",
            terminal_text(view.requester.name.as_deref().unwrap_or("unknown"))
        ),
    )?;
    if let Some(reason) = &view.reason {
        write_line(out, &format!("reason:    {}", terminal_text(reason)))?;
    }
    if view.two_person {
        write_line(
            out,
            "approver:  someone other than the requester must approve",
        )?;
    }
    write_line(out, &format!("asked:     {}", view.asked_at.to_rfc3339()))?;
    write_line(out, &format!("expires:   {}", view.expires_at.to_rfc3339()))?;
    write_line(out, &format!("answer at: {}", terminal_text(&view.url)))?;
    write_line(out, &decision_line(view))
}

fn decision_line(view: &ApprovalView) -> String {
    if view.state == ApprovalState::Pending {
        return "state:     pending, no decision yet".into();
    }
    let mut line = format!("state:     {}", view.state.label());
    if let Some(name) = view
        .decided_by
        .as_ref()
        .and_then(|person| person.name.as_deref())
    {
        line.push_str(&format!(" by {}", terminal_text(name)));
    }
    if let Some(at) = view.decided_at {
        line.push_str(&format!(" at {}", at.to_rfc3339()));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use cloudthinker_client::{
        ApprovalKind, ApprovalObject, ApprovalPerson, ApprovalSourceStatus, Unreported,
    };
    use uuid::Uuid;

    fn view(state: ApprovalState, decider: Option<&str>) -> ApprovalView {
        let at = Utc.with_ymd_and_hms(2026, 11, 6, 19, 25, 2).unwrap();
        ApprovalView {
            id: Unreported,
            request_id: Uuid::from_u128(1),
            kind: ApprovalKind::AgentCliWrite,
            state,
            source_status: ApprovalSourceStatus::Pending,
            title: "Scale payments-api\u{1b}[2J".into(),
            object: ApprovalObject {
                reference: None,
                label: "kubectl scale deploy/payments-api --replicas 6".into(),
                incident: false,
            },
            requester: ApprovalPerson {
                user_id: None,
                name: Some("Steve Tran".into()),
            },
            approver: Unreported,
            routing: Unreported,
            reason: None,
            two_person: true,
            asked_at: at,
            escalate_at: Unreported,
            expires_at: at,
            plan_revision: Unreported,
            terraform_plan_sha256: Unreported,
            scope: Unreported,
            decided_by: decider.map(|name| ApprovalPerson {
                user_id: None,
                name: Some(name.into()),
            }),
            decided_surface: Unreported,
            decided_at: decider.map(|_| at),
            decision: Unreported,
            deliveries: Unreported,
            advisor: Unreported,
            url: "https://app.example.com/approvals/1".into(),
        }
    }

    fn rendered(render: impl FnOnce(&mut Vec<u8>) -> Result<(), String>) -> String {
        let mut out = Vec::new();
        render(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_page_renders_one_tab_separated_row_per_request_without_control_bytes() {
        let page = ApprovalPage {
            items: vec![view(ApprovalState::Pending, None)],
            next_cursor: None,
        };
        assert_eq!(
            rendered(|out| render_page(out, &page)),
            "REQUEST ID\tKIND\tASKED AT\tEXPIRES AT\tREQUESTER\tTITLE\n00000000-0000-0000-0000-000000000001\tagent CLI write\t2026-11-06T19:25:02+00:00\t2026-11-06T19:25:02+00:00\tSteve Tran\tScale payments-api[2J\n"
        );
    }

    #[test]
    fn a_request_block_ends_with_its_state_line() {
        let text = rendered(|out| render_request(out, &view(ApprovalState::Pending, None)));
        assert!(text.contains("approver:  someone other than the requester must approve\n"));
        assert!(text.contains("answer at: https://app.example.com/approvals/1\n"));
        assert!(text.ends_with("state:     pending, no decision yet\n"));
    }

    #[test]
    fn a_decision_line_names_the_decider_and_time() {
        assert_eq!(
            decision_line(&view(ApprovalState::Declined, Some("Linh Tran"))),
            "state:     declined by Linh Tran at 2026-11-06T19:25:02+00:00"
        );
        assert_eq!(
            decision_line(&view(ApprovalState::Expired, None)),
            "state:     expired"
        );
    }
}
