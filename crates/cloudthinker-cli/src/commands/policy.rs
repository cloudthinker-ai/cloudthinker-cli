use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use cloudthinker_client::{
    CtError, POLICY_DOCUMENT_MAX_BYTES, PolicyImportMode, PolicyImportOptions,
};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::build_client;

pub async fn run_export(base_url: &str, workspace: Option<&str>, path: Option<&Path>) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    match client.export_policy().await {
        Ok(yaml) => exit::finish_output(output::emit_policy_export(&yaml, path)),
        Err(error) => exit::report(&error),
    }
}

pub async fn run_import(
    base_url: &str,
    workspace: Option<&str>,
    path: &Path,
    options: PolicyImportOptions,
    json: bool,
) -> ExitCode {
    let content = match read_policy(path) {
        Ok(content) => content,
        Err(error) => return exit::report(&error),
    };
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return exit::report(&error),
    };
    let report = match client.import_policy(&content, options).await {
        Ok(report) => report,
        Err(error) => {
            if options.mode == PolicyImportMode::Apply
                && matches!(
                    &error,
                    CtError::Transport(_) | CtError::Protocol(_) | CtError::Timeout(_)
                )
            {
                output::progress(
                    "Apply outcome may be unknown. Export the Policy and preview again before applying.",
                );
            }
            return exit::report(&error);
        }
    };
    if let Err(error) = output::emit_policy_import(&report, json) {
        if report.applied {
            output::progress("The Policy was applied, but its report could not be written.");
        }
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    if !report.errors.is_empty() || report.counts.invalid > 0 {
        return exit::report(&CtError::Usage(
            "Policy import validation failed. Fix the reported issues and preview again.".into(),
        ));
    }
    if options.mode == PolicyImportMode::Apply && !report.applied {
        return exit::report(&CtError::Protocol(
            "the server did not apply the Policy; preview again before applying".into(),
        ));
    }
    ExitCode::Ok
}

fn read_policy(path: &Path) -> Result<String, CtError> {
    let path_label = output::terminal_text(&path.display().to_string());
    let failure = |error| CtError::Usage(format!("Could not read {path_label}: {error}"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed());
    }
    let file = options.open(path).map_err(failure)?;
    let metadata = file.metadata().map_err(failure)?;
    if !metadata.is_file() {
        return Err(CtError::Usage(format!(
            "Policy input must be a regular UTF-8 file: {path_label}"
        )));
    }
    let oversized = || {
        CtError::Usage(format!(
            "Policy input exceeds {POLICY_DOCUMENT_MAX_BYTES} bytes: {path_label}"
        ))
    };
    if metadata.len() > POLICY_DOCUMENT_MAX_BYTES as u64 {
        return Err(oversized());
    }
    let mut bytes = Vec::new();
    file.take(POLICY_DOCUMENT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() > POLICY_DOCUMENT_MAX_BYTES {
        return Err(oversized());
    }
    String::from_utf8(bytes)
        .map_err(|_| CtError::Usage(format!("Policy input must be UTF-8: {path_label}")))
}
