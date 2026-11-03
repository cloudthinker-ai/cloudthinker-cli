use std::fs;
use std::io::{self, Write};
use std::path::Path;

use cloudthinker_client::PolicyImportReport;

use super::{emit_json, progress, terminal_text, write_line};

pub fn emit_policy_export(yaml: &str, path: Option<&Path>) -> Result<(), String> {
    let Some(path) = path else {
        let mut out = io::stdout().lock();
        return match out.write_all(yaml.as_bytes()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    };
    let path_label = terminal_text(&path.display().to_string());
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(format!(
                "Output path already exists: {path_label}. Choose a new path."
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not inspect {path_label}: {error}")),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let failure = |error| format!("Could not write {path_label}: {error}");
    let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
    staged.write_all(yaml.as_bytes()).map_err(failure)?;
    staged.as_file().sync_all().map_err(failure)?;
    staged
        .persist_noclobber(path)
        .map_err(|error| failure(error.error))?;
    progress(&format!("Wrote {path_label}"));
    Ok(())
}

pub fn emit_policy_import(report: &PolicyImportReport, json: bool) -> Result<(), String> {
    if json {
        return emit_json(report);
    }
    let mut out = io::stdout().lock();
    write_line(
        &mut out,
        &format!("mode {} applied={}", report.mode.label(), report.applied),
    )?;
    let counts = &report.counts;
    write_line(
        &mut out,
        &format!(
            "counts created={} changed={} removed={} unchanged={} activated={} invalid={}",
            counts.created,
            counts.changed,
            counts.removed,
            counts.unchanged,
            counts.activated,
            counts.invalid
        ),
    )?;
    for rule in &report.rules {
        write_line(
            &mut out,
            &format!(
                "{} {} [{}]: {} -> {} fields={}",
                terminal_text(&rule.change),
                terminal_text(&rule.slug),
                terminal_text(&rule.effect),
                terminal_text(&rule.sentence),
                terminal_text(&rule.becomes),
                terminal_text(&rule.fields.join(", "))
            ),
        )?;
    }
    for section in &report.sections {
        write_line(
            &mut out,
            &format!(
                "{} {}.{}: {} -> {}",
                terminal_text(&section.change),
                terminal_text(&section.section),
                terminal_text(&section.key),
                optional_text(section.before.as_deref()),
                optional_text(section.after.as_deref())
            ),
        )?;
    }
    for issue in &report.errors {
        let allowed = if issue.allowed_values.is_empty() {
            String::new()
        } else {
            format!(
                " (allowed: {})",
                terminal_text(&issue.allowed_values.join(", "))
            )
        };
        write_line(
            &mut out,
            &format!(
                "error {} path={} line={} column={} slug={}: {}{}",
                terminal_text(&issue.code),
                terminal_text(&issue.path),
                issue
                    .line
                    .map_or_else(|| "-".into(), |value| value.to_string()),
                issue
                    .column
                    .map_or_else(|| "-".into(), |value| value.to_string()),
                optional_text(issue.slug.as_deref()),
                terminal_text(&issue.message),
                allowed
            ),
        )?;
    }
    Ok(())
}

fn optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "-".into(), terminal_text)
}
