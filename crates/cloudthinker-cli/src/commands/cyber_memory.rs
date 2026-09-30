use std::fs::{self, symlink_metadata};
use std::path::{Component, Path};

use cloudthinker_client::CyberMemorySnapshot;
use serde::Serialize;
use uuid::Uuid;

use crate::commands::build_client;
use crate::engine::exit::ExitCode;
use crate::engine::output;

#[derive(Debug, Serialize)]
struct PullSummary {
    app_id: Uuid,
    findings: usize,
    surface_items: usize,
    context_sources: usize,
    downloaded_context_files: usize,
    output_dir: String,
}

pub async fn pull(
    base_url: &str,
    workspace: Option<&str>,
    app_id: Uuid,
    output_dir: &Path,
    include_context: bool,
    json: bool,
) -> ExitCode {
    let client = match build_client(base_url, workspace) {
        Ok(client) => client,
        Err(error) => return crate::engine::exit::report(&error),
    };
    let snapshot = match client.cyber_pull_memory(app_id, include_context).await {
        Ok(snapshot) => snapshot,
        Err(error) => return crate::engine::exit::report(&error),
    };
    let summary = match save_snapshot(output_dir, &snapshot) {
        Ok(summary) => summary,
        Err(error) => {
            output::eprintln_error(&error.to_string());
            return ExitCode::JobFailed;
        }
    };
    let result = if json {
        output::emit_json(&summary)
    } else {
        output::print_lines(&[format!(
            "Pulled {} findings, {} surface items, and {} context sources to {}",
            summary.findings,
            summary.surface_items,
            summary.context_sources,
            output::terminal_text(&summary.output_dir)
        )])
    };
    match result {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            output::eprintln_error(&error);
            ExitCode::JobFailed
        }
    }
}

fn save_snapshot(
    output_dir: &Path,
    snapshot: &CyberMemorySnapshot,
) -> std::io::Result<PullSummary> {
    ensure_directory(output_dir)?;
    let context_dir = output_dir.join("context");
    ensure_directory(&context_dir)?;
    let findings_path = output_dir.join("findings.json");
    let surface_path = output_dir.join("surface.json");
    let sources_path = context_dir.join("sources.json");
    output::write_json_file(&findings_path, &snapshot.findings).map_err(std::io::Error::other)?;
    output::write_json_file(&surface_path, &snapshot.surface).map_err(std::io::Error::other)?;
    output::write_json_file(&sources_path, &snapshot.context_sources)
        .map_err(std::io::Error::other)?;
    for file in &snapshot.context_files {
        let relative = Path::new(&file.relative_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "context file path is not relative and normalized",
            ));
        }
        let target = output_dir.join(relative);
        let parent = target.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "context file has no parent",
            )
        })?;
        ensure_directory(parent)?;
        output::write_file(&target, &file.bytes).map_err(std::io::Error::other)?;
    }
    Ok(PullSummary {
        app_id: snapshot.app_id,
        findings: snapshot.findings.len(),
        surface_items: snapshot.surface.len(),
        context_sources: snapshot.context_sources.len(),
        downloaded_context_files: snapshot.context_files.len(),
        output_dir: output_dir.display().to_string(),
    })
}

fn ensure_directory(path: &Path) -> std::io::Result<()> {
    match symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "refusing to use symlink output directory {}",
                path.display()
            ),
        )),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("output path is not a directory: {}", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            let metadata = symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("refusing unsafe output directory {}", path.display()),
                ));
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use cloudthinker_client::{CyberMemoryContextSource, CyberMemoryFile};
    use tempfile::tempdir;

    fn snapshot() -> CyberMemorySnapshot {
        CyberMemorySnapshot {
            app_id: Uuid::new_v4(),
            findings: Vec::new(),
            surface: Vec::new(),
            context_sources: vec![CyberMemoryContextSource {
                source_id: Uuid::new_v4(),
                source_type: "openapi".into(),
                name: "schema.json".into(),
                status: "ready".into(),
                created_at: Utc::now(),
                local_file: Some("context/files/schema.json".into()),
                sha256: Some("abc".into()),
            }],
            context_files: vec![CyberMemoryFile {
                relative_path: "context/files/schema.json".into(),
                content_type: "application/json".into(),
                sha256: "abc".into(),
                bytes: b"{}".to_vec(),
            }],
        }
    }

    #[test]
    fn pull_snapshot_round_trips_context_and_metadata() {
        let temp = tempdir().unwrap();
        let output_dir = temp.path().join("memory");
        let snapshot = snapshot();
        let summary = save_snapshot(&output_dir, &snapshot).unwrap();
        assert_eq!(summary.context_sources, 1);
        assert_eq!(summary.downloaded_context_files, 1);
        assert_eq!(
            fs::read(output_dir.join("context/files/schema.json")).unwrap(),
            b"{}"
        );
        let sources: Vec<CyberMemoryContextSource> =
            serde_json::from_slice(&fs::read(output_dir.join("context/sources.json")).unwrap())
                .unwrap();
        assert_eq!(
            sources[0].local_file.as_deref(),
            Some("context/files/schema.json")
        );
        assert!(output_dir.join("findings.json").is_file());
        assert!(output_dir.join("surface.json").is_file());
        fs::write(output_dir.join("findings.json"), b"stale").unwrap();
        save_snapshot(&output_dir, &snapshot).unwrap();
        assert_eq!(fs::read(output_dir.join("findings.json")).unwrap(), b"[]");
    }

    #[cfg(unix)]
    #[test]
    fn pull_rejects_symlink_output_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let actual = temp.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let link = temp.path().join("link");
        symlink(&actual, &link).unwrap();
        assert!(ensure_directory(&link).is_err());
        assert!(actual.read_dir().unwrap().next().is_none());
    }
}
