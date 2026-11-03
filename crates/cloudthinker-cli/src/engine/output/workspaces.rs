use std::io::Write;

use cloudthinker_client::Workspaces;

use super::{emit_json_to, terminal_text, write_line};

pub fn emit_workspaces(
    workspaces: &Workspaces,
    login: &str,
    selection_hint: bool,
    json: bool,
) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    if json {
        return emit_json_to(&mut out, &workspaces.data);
    }
    render_workspaces(&mut out, workspaces, login, selection_hint)
}

fn render_workspaces(
    out: &mut impl Write,
    workspaces: &Workspaces,
    login: &str,
    selection_hint: bool,
) -> Result<(), String> {
    if workspaces.data.is_empty() {
        return write_line(
            out,
            &format!(
                "No workspaces found in the organization of your current login. Check it with cloudthinker whoami, or run {login} to sign in to another organization."
            ),
        );
    }
    write_line(out, "SELECTED\tWORKSPACE ID\tNAME\tROLE\tSAVED LOGIN")?;
    for workspace in &workspaces.data {
        let saved = match workspace.saved_login {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        write_line(
            out,
            &format!(
                "{}\t{}\t{}\t{}\t{}",
                if workspace.selected { "*" } else { "" },
                workspace.id,
                terminal_text(&workspace.name),
                terminal_text(&workspace.role),
                saved
            ),
        )?;
    }
    let scope = workspaces.organization_name().map_or_else(
        || "Only workspaces in the organization of your current login are listed.".to_string(),
        |org| {
            format!(
                "Only workspaces in {}, the organization of your current login, are listed.",
                terminal_text(org)
            )
        },
    );
    write_line(out, &scope)?;
    if !workspaces.includes_selected() {
        write_line(
            out,
            &format!(
                "Your selected workspace {} is not in this list.",
                workspaces.selected_id
            ),
        )?;
    }
    if selection_hint {
        write_line(
            out,
            "Use --workspace <ID> with a saved login, or cloudthinker auth switch <ID> to make it the default.",
        )?;
    }
    write_line(
        out,
        &format!(
            "For a workspace without a saved login, or another organization, run {login} and choose it in the browser."
        ),
    )
}

#[cfg(test)]
mod tests {
    use cloudthinker_client::WorkspaceView;
    use uuid::Uuid;

    use super::*;

    fn row(id: u128, name: &str, selected: bool, saved_login: Option<bool>) -> WorkspaceView {
        WorkspaceView {
            id: Uuid::from_u128(id),
            name: name.into(),
            role: "admin".into(),
            organization_name: Some("Acme".into()),
            is_default: false,
            selected,
            saved_login,
        }
    }

    fn render_with(workspaces: &Workspaces, selection_hint: bool) -> String {
        let mut out = Vec::new();
        render_workspaces(&mut out, workspaces, "cloudthinker login", selection_hint).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn render(workspaces: &Workspaces) -> String {
        render_with(workspaces, true)
    }

    #[test]
    fn workspaces_render_one_row_each_and_mark_the_selected_one() {
        let workspaces = Workspaces {
            selected_id: Uuid::from_u128(1),
            data: vec![
                row(2, "Dev\u{1b}[31m", false, None),
                row(1, "Prod", true, Some(true)),
            ],
        };

        assert_eq!(
            render(&workspaces),
            "SELECTED\tWORKSPACE ID\tNAME\tROLE\tSAVED LOGIN\n\
             \t00000000-0000-0000-0000-000000000002\tDev[31m\tadmin\tunknown\n\
             *\t00000000-0000-0000-0000-000000000001\tProd\tadmin\tyes\n\
             Only workspaces in Acme, the organization of your current login, are listed.\n\
             Use --workspace <ID> with a saved login, or cloudthinker auth switch <ID> to make it the default.\n\
             For a workspace without a saved login, or another organization, run cloudthinker login and choose it in the browser.\n"
        );
    }

    #[test]
    fn workspaces_render_an_empty_list_as_one_hint() {
        let workspaces = Workspaces {
            selected_id: Uuid::from_u128(1),
            data: Vec::new(),
        };

        assert_eq!(
            render(&workspaces),
            "No workspaces found in the organization of your current login. Check it with cloudthinker whoami, or run cloudthinker login to sign in to another organization.\n"
        );
    }

    #[test]
    fn workspaces_render_notes_a_selected_workspace_missing_from_the_list() {
        let workspaces = Workspaces {
            selected_id: Uuid::from_u128(9),
            data: vec![row(2, "Dev", false, Some(false))],
        };

        assert!(render(&workspaces).contains(
            "Your selected workspace 00000000-0000-0000-0000-000000000009 is not in this list."
        ));
    }

    #[test]
    fn workspaces_render_omits_the_workspace_hint_when_a_token_overrides_it() {
        let workspaces = Workspaces {
            selected_id: Uuid::from_u128(1),
            data: vec![row(1, "Prod", true, Some(true))],
        };

        let text = render_with(&workspaces, false);

        assert!(!text.contains("--workspace"));
        assert!(text.contains("run cloudthinker login and choose it in the browser."));
    }

    #[test]
    fn workspaces_json_is_a_stable_array() {
        let workspaces = [row(1, "Prod", true, Some(true))];
        let mut out = Vec::new();
        emit_json_to(&mut out, &workspaces).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "[\n  {\n    \"id\": \"00000000-0000-0000-0000-000000000001\",\n    \"name\": \"Prod\",\n    \"role\": \"admin\",\n    \"organization_name\": \"Acme\",\n    \"is_default\": false,\n    \"selected\": true,\n    \"saved_login\": true\n  }\n]\n"
        );
    }
}
