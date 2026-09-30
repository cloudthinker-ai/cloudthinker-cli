//! Cyber's local setup facts inside the shared CLI project config.
//!
//! The file is `.cloudthinker/config.toml` (project) or `~/.cloudthinker/config.toml`
//! (user), and Cyber owns the `[cyber]` and `[cyber.auth]` tables inside it. The
//! `cyber config` surface and `resolved_values` speak leaf keys (`app`,
//! `auth.primary`); the `cyber.` prefix is added on write and stripped on read,
//! so the file can carry other features' tables beside Cyber's.
//!
//! `[cyber.auth]` maps a test identity's role to the environment variable that
//! holds its auth header (`primary = "CYBER_PRIMARY_AUTH"`), so an authorization
//! test drives several identities from one line each. Every value under `auth.`
//! is an env-var name, never a secret; the header's shape stays in the env.
//!
//! This is intentionally a small line-oriented TOML editor. Preserving the
//! original lines keeps comments and unrelated keys intact without introducing a
//! second configuration parser.
//!
//! A fact resolves by precedence: an explicit config value wins over an
//! auto-derived default (see `effective_values`), so the file stays one line in
//! the common case yet every derived value stays overridable.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::engine::{exit::ExitCode, output};

/// The table Cyber owns inside the shared project config.
const CYBER_PREFIX: &str = "cyber";

/// The identities table. Every value beneath it names an env var holding an
/// auth header, so the guard and the preflight both key on this one prefix.
const AUTH_PREFIX: &str = "auth.";

/// The context memo table. Every value beneath it names a path or URL of a
/// context artifact (an OpenAPI spec, a Postman collection, a HAR, or prose).
const CONTEXT_PREFIX: &str = "context.";

/// An authorization test needs at least this many identities: one to act as and
/// one to be denied against. `cyber doctor` warns below it.
pub const BOLA_MIN_IDENTITIES: usize = 2;

#[derive(Debug, Clone, Serialize)]
pub struct CyberConfigView {
    pub project_file: String,
    pub user_file: String,
    pub values: BTreeMap<String, String>,
    pub derived: BTreeMap<String, String>,
}

pub fn show(json: bool) -> ExitCode {
    let explicit = resolved_values();
    let derived = derived_only(&explicit);
    let view = CyberConfigView {
        project_file: project_path().display().to_string(),
        user_file: user_path().display().to_string(),
        values: explicit,
        derived,
    };
    let result = if json {
        output::emit_json(&view)
    } else {
        print_view(&view)
    };
    finish(result)
}

pub fn get(key: &str, json: bool) -> ExitCode {
    if !valid_key(key) {
        output::eprintln_error("config key must contain only letters, digits, `_`, `.`, or `-`");
        return ExitCode::Usage;
    }
    let leaf = strip_prefix(key);
    let value = effective_values().get(leaf).cloned();
    let result = if json {
        output::emit_json(&serde_json::json!({"key": leaf, "value": value}))
    } else {
        match value {
            Some(value) => output::print_config_value(leaf, &value),
            None => {
                output::eprintln_error(&format!("config key `{leaf}` is not set"));
                return ExitCode::JobFailed;
            }
        }
    };
    finish(result)
}

pub fn set(key: &str, value: &str, user: bool, json: bool) -> ExitCode {
    if !valid_key(key) {
        output::eprintln_error("config key must contain only letters, digits, `_`, `.`, or `-`");
        return ExitCode::Usage;
    }
    let leaf = strip_prefix(key);
    if is_auth_key(leaf) && !valid_env_name(value) {
        output::eprintln_error(
            "an identity under [cyber.auth] stores an environment-variable name, not a secret",
        );
        return ExitCode::Usage;
    }
    let path = if user { user_path() } else { project_path() };
    if let Err(error) = write_key(&path, leaf, value) {
        output::eprintln_error(&error);
        return ExitCode::JobFailed;
    }
    let result = if json {
        output::emit_json(&serde_json::json!({
            "key": leaf,
            "value": value,
            "scope": if user { "user" } else { "project" },
            "file": path,
        }))
    } else {
        output::print_config_value(leaf, value)
    };
    finish(result)
}

/// Write a Cyber leaf key to the project config without printing. The caller is
/// a command that resolved the value itself, such as `cyber run open` saving
/// the App mapping so the next run needs no `--app`.
pub fn set_project_key(key: &str, value: &str) -> Result<(), String> {
    write_key(&project_path(), strip_prefix(key), value)
}

fn write_key(path: &Path, leaf: &str, value: &str) -> Result<(), String> {
    let full_key = format!("{CYBER_PREFIX}.{leaf}");
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        return Err(format!("cannot create {}: {error}", parent.display()));
    }
    let content = fs::read_to_string(path).unwrap_or_default();
    let updated = upsert_line(&content, &full_key, value);
    fs::write(path, updated).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn project_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".cloudthinker")
        .join("config.toml")
}

fn user_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cloudthinker")
        .join("config.toml")
}

/// The Cyber facts written explicitly in either config file, project over user,
/// keyed by leaf (`app`, `auth.primary`). Never carries a derived default.
pub fn resolved_values() -> BTreeMap<String, String> {
    let mut merged = read_values(&user_path());
    merged.extend(read_values(&project_path()));
    strip_cyber(&merged)
}

/// The Cyber facts a run actually uses: an explicit value wins over the derived
/// default beneath it. Credentials are never derived, only read.
pub fn effective_values() -> BTreeMap<String, String> {
    effective_from(resolved_values(), derived_defaults())
}

fn effective_from(
    explicit: BTreeMap<String, String>,
    derived: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut effective = derived;
    effective.extend(explicit);
    effective
}

/// Defaults the CLI can compute from the machine, so the file need not carry
/// them. `repo_path` is the git worktree root the gray-box source lives under.
fn derived_defaults() -> BTreeMap<String, String> {
    let mut derived = BTreeMap::new();
    if let Some(root) = git_root() {
        derived.insert("repo_path".to_string(), root);
    }
    derived
}

fn derived_only(explicit: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    derived_defaults()
        .into_iter()
        .filter(|(key, _)| !explicit.contains_key(key))
        .collect()
}

fn git_root() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    (!root.is_empty()).then(|| root.to_string())
}

/// Drop the `cyber.` prefix so callers speak leaf keys; a key in another table
/// is not a Cyber fact and is excluded.
fn strip_cyber(merged: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    merged
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(&format!("{CYBER_PREFIX}."))
                .map(|leaf| (leaf.to_string(), value.clone()))
        })
        .collect()
}

fn strip_prefix(key: &str) -> &str {
    key.strip_prefix(&format!("{CYBER_PREFIX}.")).unwrap_or(key)
}

/// A leaf under the identities table. Every such value names an env var, so the
/// role and the auth method never appear as config structure.
fn is_auth_key(leaf: &str) -> bool {
    leaf.starts_with(AUTH_PREFIX)
}

/// Every configured identity whose env var is unset or empty, as `key=ENV`. A
/// local run blocks on this rather than degrading a gray/white-box mode to
/// black-box; a run with no identities returns an empty list.
pub fn missing_secret_env_vars() -> Vec<String> {
    missing_from(&resolved_values(), |name| {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    })
}

/// The auth posture a local run declares, derived from context, plus the reason
/// to show the engineer. Prefer white; defer to what this machine can supply.
/// White needs readable source and a configured identity; gray needs an
/// identity; otherwise black. The backend clamps this to the App's allowed
/// modes, and the missing-env preflight still blocks a run whose identity is
/// unset.
pub fn derived_mode() -> (cloudthinker_client::CyberMode, &'static str) {
    let values = effective_values();
    let has_source = values
        .get("repo_path")
        .is_some_and(|path| Path::new(path).is_dir());
    let has_identity = !auth_identities().is_empty();
    mode_for(has_source, has_identity)
}

fn mode_for(
    has_source: bool,
    has_identity: bool,
) -> (cloudthinker_client::CyberMode, &'static str) {
    use cloudthinker_client::CyberMode;

    match (has_source, has_identity) {
        (true, true) => (CyberMode::White, "repo_path readable + identity configured"),
        (false, true) => (
            CyberMode::Gray,
            "identity configured, no readable repo_path",
        ),
        _ => (CyberMode::Black, "no identity configured"),
    }
}

/// Each configured identity as `(role, env-var-name)`. The role is the leaf
/// under the identities table; the value names the env var holding its auth
/// header. `cyber doctor` reports whether each env var is set.
pub fn auth_identities() -> Vec<(String, String)> {
    resolved_values()
        .into_iter()
        .filter(|(key, _)| is_auth_key(key))
        .map(|(key, env)| (key.trim_start_matches(AUTH_PREFIX).to_string(), env))
        .collect()
}

/// Each context memo as `(label, path-or-URL)`. The label is the leaf under the
/// context table; the value points at a spec, a collection, a capture, or a
/// prose doc. `cyber doctor` reports whether each one resolves on this machine.
pub fn context_items() -> Vec<(String, String)> {
    resolved_values()
        .into_iter()
        .filter(|(key, _)| is_context_key(key))
        .map(|(key, source)| (key.trim_start_matches(CONTEXT_PREFIX).to_string(), source))
        .collect()
}

/// A leaf under the context memo table. The value is a path or URL, not a
/// secret, so it carries no env-var guard.
fn is_context_key(leaf: &str) -> bool {
    leaf.starts_with(CONTEXT_PREFIX)
}

/// Resolve an explicitly selected identity, `anonymous`, or the default role.
pub fn probe_auth_header(identity: Option<&str>) -> Result<Option<(String, String)>, String> {
    let identities = auth_identities();
    if identity == Some("anonymous") {
        return Ok(None);
    }
    let chosen = match identity {
        Some(role) => Some(
            identities
                .iter()
                .find(|(name, _)| name == role)
                .ok_or_else(|| format!("Cyber identity `{role}` is not configured"))?,
        ),
        None => ["owner", "primary"]
            .iter()
            .find_map(|role| identities.iter().find(|(name, _)| name == role))
            .or_else(|| identities.first()),
    };
    let Some((role, env)) = chosen else {
        return Ok(None);
    };
    configured_probe_header(role, std::env::var(env).ok())
}

fn configured_probe_header(
    role: &str,
    raw: Option<String>,
) -> Result<Option<(String, String)>, String> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Err(format!(
            "Cyber identity `{role}` environment value is missing"
        ));
    };
    parse_header_line(&raw)
        .map(Some)
        .ok_or_else(|| format!("Cyber identity `{role}` must contain a `Header-Name: value` line"))
}

/// Split a stored header line such as `Authorization: Bearer eyJ...` into its
/// name and value.
fn parse_header_line(raw: &str) -> Option<(String, String)> {
    let (name, value) = raw.split_once(':')?;
    let name = name.trim();
    let value = value.trim();
    (!name.is_empty() && !value.is_empty()).then(|| (name.to_string(), value.to_string()))
}

/// The project config path as a display string, for a readiness report.
pub fn project_config_path() -> String {
    project_path().display().to_string()
}

fn missing_from(values: &BTreeMap<String, String>, is_set: impl Fn(&str) -> bool) -> Vec<String> {
    values
        .iter()
        .filter(|(key, _)| is_auth_key(key))
        .filter(|(_, env)| !is_set(env))
        .map(|(key, env)| format!("{key}={env}"))
        .collect()
}

fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
}

fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase() || ch == '_')
}

fn read_values(path: &Path) -> BTreeMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let mut section = None;
    let mut values = BTreeMap::new();
    for line in content.lines() {
        if let Some(name) = parse_section(line) {
            section = Some(name);
            continue;
        }
        if let Some((key, value)) = parse_line(line) {
            let full_key = section
                .as_ref()
                .map_or_else(|| key.clone(), |name| format!("{name}.{key}"));
            values.insert(full_key, value);
        }
    }
    values
}

fn parse_section(line: &str) -> Option<String> {
    let trimmed = line.trim();
    trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .map(str::trim)
        .filter(|value| valid_key(value))
        .map(str::to_string)
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let without_comment = line.split_once('#').map_or(line, |(left, _)| left).trim();
    let (key, raw) = without_comment.split_once('=')?;
    let key = key.trim();
    if !valid_key(key) {
        return None;
    }
    let value = raw.trim().trim_matches('"').trim_matches('\'');
    Some((key.to_string(), value.to_string()))
}

fn upsert_line(content: &str, key: &str, value: &str) -> String {
    let (section, leaf) = key
        .rsplit_once('.')
        .map_or((None, key), |(section, leaf)| (Some(section), leaf));
    let encoded = format!("{} = {}", leaf, toml_quote(value));
    let mut found = false;
    let mut active_section = None;
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    for line in &mut lines {
        if let Some(name) = parse_section(line) {
            active_section = Some(name);
            continue;
        }
        let in_section = active_section.as_deref() == section;
        if in_section && parse_line(line).is_some_and(|(existing, _)| existing == leaf) {
            found = true;
            *line = preserve_inline_comment(line, &encoded);
        }
    }
    if !found {
        if let Some(section) = section {
            let mut insert_at = lines.len();
            let mut in_section = false;
            for (index, line) in lines.iter().enumerate() {
                if parse_section(line).as_deref() == Some(section) {
                    in_section = true;
                    continue;
                }
                if in_section && line.trim_start().starts_with('[') {
                    insert_at = index;
                    break;
                }
            }
            if in_section {
                lines.insert(insert_at, encoded);
            } else {
                if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
                    lines.push(String::new());
                }
                lines.push(format!("[{section}]"));
                lines.push(encoded);
            }
        } else {
            let insert_at = lines
                .iter()
                .position(|line| parse_section(line).is_some())
                .unwrap_or(lines.len());
            lines.insert(insert_at, encoded);
        }
    }
    let mut result = lines.join("\n");
    result.push('\n');
    result
}

fn preserve_inline_comment(original: &str, replacement: &str) -> String {
    original.split_once('#').map_or_else(
        || replacement.to_string(),
        |(_, comment)| format!("{replacement} #{}", comment),
    )
}

fn toml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn print_view(view: &CyberConfigView) -> Result<(), String> {
    let mut lines = vec![
        format!("project: {}", view.project_file),
        format!("user:    {}", view.user_file),
    ];
    lines.extend(
        view.values
            .iter()
            .map(|(key, value)| format!("{key} = {value}")),
    );
    lines.extend(
        view.derived
            .iter()
            .map(|(key, value)| format!("{key} = {value} (auto)")),
    );
    output::print_lines(&lines)
}

fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            output::eprintln_error(&error);
            ExitCode::JobFailed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        configured_probe_header, effective_from, is_auth_key, is_context_key, missing_from,
        mode_for, parse_header_line, parse_line, parse_section, read_values, strip_cyber,
        strip_prefix, upsert_line, valid_env_name,
    };
    use cloudthinker_client::CyberMode;
    use std::collections::BTreeMap;

    #[test]
    fn source_and_identity_derive_white() {
        assert_eq!(mode_for(true, true).0, CyberMode::White);
    }

    #[test]
    fn identity_without_source_derives_gray() {
        assert_eq!(mode_for(false, true).0, CyberMode::Gray);
    }

    #[test]
    fn no_identity_derives_black_even_with_source() {
        assert_eq!(mode_for(true, false).0, CyberMode::Black);
        assert_eq!(mode_for(false, false).0, CyberMode::Black);
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn setting_preserves_comments_and_unrelated_lines() {
        let before = "# defaults\ncredential_env = \"OLD\" # keep this note\nmode = \"gray\"\n";
        let after = upsert_line(before, "credential_env", "NEW");
        assert_eq!(
            after,
            "# defaults\ncredential_env = \"NEW\" # keep this note\nmode = \"gray\"\n"
        );
    }

    #[test]
    fn setting_appends_a_new_key() {
        assert_eq!(
            upsert_line("mode = \"gray\"\n", "target", "https://a"),
            "mode = \"gray\"\ntarget = \"https://a\"\n"
        );
    }

    #[test]
    fn flat_key_stays_outside_existing_sections() {
        let updated = upsert_line("[auth]\ntest_user_env = \"USER\"\n", "mode", "gray");
        assert_eq!(
            updated,
            "mode = \"gray\"\n[auth]\ntest_user_env = \"USER\"\n"
        );
    }

    #[test]
    fn parser_ignores_comments_and_secret_names_are_not_values() {
        assert_eq!(
            parse_line("credential_env = \"TEST_TOKEN\" # name"),
            Some(("credential_env".into(), "TEST_TOKEN".into()))
        );
        assert!(valid_env_name("TEST_TOKEN"));
        assert!(!valid_env_name("token-value"));
    }

    #[test]
    fn a_header_line_splits_into_name_and_value() {
        assert_eq!(
            parse_header_line("Authorization: Bearer eyJ.abc"),
            Some(("Authorization".to_string(), "Bearer eyJ.abc".to_string()))
        );
        assert_eq!(
            parse_header_line("Cookie: session=xyz"),
            Some(("Cookie".to_string(), "session=xyz".to_string()))
        );
        // A line with no colon or an empty side yields no header.
        assert_eq!(parse_header_line("Bearer eyJ.abc"), None);
        assert_eq!(parse_header_line(": value"), None);
    }

    #[test]
    fn a_configured_default_identity_must_have_a_valid_environment_header() {
        assert!(configured_probe_header("primary", None).is_err());
        assert!(configured_probe_header("primary", Some(String::new())).is_err());
        assert!(configured_probe_header("primary", Some("Bearer token".into())).is_err());
        assert_eq!(
            configured_probe_header("primary", Some("Authorization: Bearer token".into())).unwrap(),
            Some(("Authorization".into(), "Bearer token".into()))
        );
    }

    #[test]
    fn a_context_key_is_neither_an_identity_nor_a_secret() {
        assert!(is_context_key("context.api_spec"));
        assert!(!is_context_key("app"));
        assert!(!is_context_key("auth.primary"));
        // A context value is a path or URL, so no env-name guard applies to it.
        assert!(!is_auth_key("context.api_spec"));
    }

    #[test]
    fn context_and_auth_share_the_one_config_shape() {
        let after = upsert_line("", "cyber.context.api_spec", "./openapi.json");
        assert!(after.contains("[cyber.context]\napi_spec = \"./openapi.json\""));
    }

    #[test]
    fn an_identity_value_must_be_an_env_var_name() {
        assert!(is_auth_key("auth.primary"));
        assert!(!is_auth_key("app"));
        // The value the guard checks is the env-var name, never the secret.
        assert!(!valid_env_name("Bearer eyJ..."));
        assert!(valid_env_name("CYBER_PRIMARY_AUTH"));
    }

    #[test]
    fn preflight_reports_every_identity_whose_env_is_unset() {
        let values = map(&[
            ("auth.primary", "CYBER_PRIMARY_AUTH"),
            ("auth.secondary", "CYBER_SECONDARY_AUTH"),
            ("app", "APP-1"),
        ]);
        // Only `primary` is set in the environment; `app` is not an identity.
        let missing = missing_from(&values, |name| name == "CYBER_PRIMARY_AUTH");
        assert_eq!(
            missing,
            vec!["auth.secondary=CYBER_SECONDARY_AUTH".to_string()]
        );
    }

    #[test]
    fn preflight_is_empty_when_no_identity_is_configured() {
        let values = map(&[("app", "APP-1"), ("repo_path", "./api")]);
        assert!(missing_from(&values, |_| false).is_empty());
    }

    #[test]
    fn sectioned_values_are_resolved_and_section_comments_survive() {
        let path = std::env::temp_dir().join(format!(
            "cloudthinker-cyber-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "# local facts\n[auth]\ntest_user_env = \"OLD\" # keep\n[context]\nnotes = \"x\"\n",
        )
        .unwrap();
        let values = read_values(&path);
        assert_eq!(values.get("auth.test_user_env"), Some(&"OLD".to_string()));
        assert_eq!(parse_section("[auth]"), Some("auth".to_string()));
        let updated = upsert_line(
            &std::fs::read_to_string(&path).unwrap(),
            "auth.test_user_env",
            "NEW",
        );
        assert!(updated.contains("test_user_env = \"NEW\" # keep"));
        assert!(updated.contains("[context]\nnotes = \"x\""));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn nested_cyber_tables_write_under_the_cyber_prefix() {
        let after = upsert_line("", "cyber.app", "APP-1");
        assert_eq!(after, "[cyber]\napp = \"APP-1\"\n");
        let after = upsert_line(&after, "cyber.auth.test_user_env", "CYBER_USER");
        assert!(after.contains("[cyber]\napp = \"APP-1\""));
        assert!(after.contains("[cyber.auth]\ntest_user_env = \"CYBER_USER\""));
    }

    #[test]
    fn strip_cyber_keeps_only_the_cyber_table_and_drops_the_prefix() {
        let merged = map(&[
            ("cyber.app", "APP-1"),
            ("cyber.auth.test_user_env", "USER"),
            ("review.repo", "owner/name"),
        ]);
        let stripped = strip_cyber(&merged);
        assert_eq!(stripped.get("app"), Some(&"APP-1".to_string()));
        assert_eq!(
            stripped.get("auth.test_user_env"),
            Some(&"USER".to_string())
        );
        assert!(!stripped.contains_key("review.repo"));
    }

    #[test]
    fn strip_prefix_accepts_a_leaf_or_a_prefixed_key() {
        assert_eq!(strip_prefix("app"), "app");
        assert_eq!(strip_prefix("cyber.app"), "app");
        assert_eq!(
            strip_prefix("cyber.auth.test_user_env"),
            "auth.test_user_env"
        );
    }

    #[test]
    fn an_explicit_value_wins_over_the_derived_default() {
        let explicit = map(&[("repo_path", "./services/api"), ("app", "APP-1")]);
        let derived = map(&[("repo_path", "/home/dev/repo")]);
        let effective = effective_from(explicit, derived);
        assert_eq!(
            effective.get("repo_path"),
            Some(&"./services/api".to_string())
        );
        assert_eq!(effective.get("app"), Some(&"APP-1".to_string()));
    }

    #[test]
    fn a_derived_default_fills_a_blank() {
        let derived = map(&[("repo_path", "/home/dev/repo")]);
        let effective = effective_from(BTreeMap::new(), derived);
        assert_eq!(
            effective.get("repo_path"),
            Some(&"/home/dev/repo".to_string())
        );
    }
}
