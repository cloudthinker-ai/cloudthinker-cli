use std::io::Read;

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use cloudthinker_client::spec::{Method, Operation, OperationTable, Param, ParamKind};
use cloudthinker_client::{
    BodyField, BodyInput, CtClient, CtError, PublicCall, QueryValue, SpecFreshness,
    operation_summary,
};

use crate::engine::confirm::{self, Consent};
use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

use super::{build_client, env_token_is_set};

const BODY_MAX_BYTES: u64 = 256 * 1024;
const ARG_WORKSPACE: &str = "workspace";
const ARG_BODY: &str = "body";
const ARG_SET: &str = "set";
const ARG_ALL: &str = "all";
const ARG_IDEMPOTENCY: &str = "key";
const ARG_IF_MATCH: &str = "etag";
const ARG_QUERY: &str = "query";
const ARG_YES: &str = "yes";
pub const REVIEW_FINDING: &str = "review.findings.get";
const FOLDED: [&str; 1] = [REVIEW_FINDING];
const HEADER_IF_MATCH: &str = "if-match";
const TYPO_SCORE: f64 = 0.85;

pub enum Target {
    Command(Vec<String>, Box<Command>),
    Api(Vec<String>),
}

pub async fn run(base_url: &str, workspace: Option<&str>, yes: bool, target: Target) -> ExitCode {
    if let Target::Command(argv, cli) = &target
        && let Some(usage) = hand_conflict(cli, argv)
    {
        return clap_exit(&usage);
    }
    let store_workspace = if env_token_is_set() { None } else { workspace };
    let client = match build_client(base_url, store_workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    let spec = match client.public_spec().await {
        Ok(spec) => spec,
        Err(err) => {
            return match target {
                Target::Command(argv, cli) => hand_typo(&cli, &argv)
                    .map_or_else(|| exit::report(&err), |usage| clap_exit(&usage)),
                Target::Api(_) => exit::report(&err),
            };
        }
    };
    if spec.freshness == SpecFreshness::Offline {
        output::warn("the server is unreachable; using the saved public API spec");
    }
    let parsed = match target {
        Target::Command(argv, _) => parse_command(&spec.table, argv),
        Target::Api(argv) => parse_api(&spec.table, argv),
    };
    let (op, matches, raw_path) = match parsed {
        Ok(parsed) => parsed,
        Err(Stop::Clap(error)) => return clap_exit(&error),
        Err(Stop::Ct(error)) => return exit::report(&error),
    };
    let explicit = matches
        .try_get_one::<String>(ARG_WORKSPACE)
        .ok()
        .flatten()
        .map(String::as_str)
        .or(workspace)
        .map(str::to_string);
    let explicit = match explicit {
        None if op.workspace_scoped && raw_path.is_none() => {
            match client.default_workspace().await {
                Ok(id) => Some(id.to_string()),
                Err(err) => return exit::report(&err),
            }
        }
        explicit => explicit,
    };
    let call = match build_call(&client, op, &matches, raw_path, explicit.as_deref()) {
        Ok(call) => call,
        Err(err) => return exit::report(&err),
    };
    let yes = yes
        || matches
            .try_get_one::<bool>(ARG_YES)
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false);
    if op.confirm
        && let Some(code) = confirm_call(op, &call, yes)
    {
        return code;
    }
    send(&client, &call).await
}

fn confirm_call(op: &Operation, call: &PublicCall, yes: bool) -> Option<ExitCode> {
    match confirm::consent_gate(yes, confirm::interactive()) {
        Consent::Given => None,
        Consent::Refuse => {
            output::eprintln_error(&format!(
                "{} asks you to confirm, and no terminal can answer. Run it again with --yes.",
                output::terminal_text(&op.id)
            ));
            Some(ExitCode::Usage)
        }
        Consent::Ask => {
            let prompt = format!(
                "Run {} {} ({})? [y/N] ",
                call.method,
                output::terminal_text(&call.path),
                output::terminal_text(&op.id)
            );
            if confirm::ask(&prompt, "nothing was sent", &mut std::io::stdin().lock()) {
                None
            } else {
                output::progress("Cancelled. Nothing was sent.");
                Some(ExitCode::JobFailed)
            }
        }
    }
}

enum Stop {
    Clap(clap::Error),
    Ct(CtError),
}

fn parse_command(
    table: &OperationTable,
    argv: Vec<String>,
) -> Result<(&Operation, ArgMatches, Option<String>), Stop> {
    let matches = command_tree(table)
        .try_get_matches_from(std::iter::once("cloudthinker".to_string()).chain(argv))
        .map_err(Stop::Clap)?;
    let mut words = Vec::new();
    let mut leaf = &matches;
    while let Some((name, sub)) = leaf.subcommand() {
        words.push(name);
        leaf = sub;
    }
    let op = table
        .operations
        .iter()
        .find(|op| op.cli.words() == words)
        .ok_or_else(unknown)?;
    Ok((op, leaf.clone(), None))
}

fn parse_api(
    table: &OperationTable,
    argv: Vec<String>,
) -> Result<(&Operation, ArgMatches, Option<String>), Stop> {
    let mut words = argv.into_iter();
    let first = words.next().ok_or_else(|| {
        Stop::Ct(CtError::Usage(
            "name an operation (`cloudthinker api resolve.incidents.list`) or a method and path (`cloudthinker api GET /v1/workspaces/<id>/resolve/incidents`)".into(),
        ))
    })?;
    if let Some(method) = Method::parse(&first) {
        let path = words
            .next()
            .ok_or_else(|| Stop::Ct(CtError::Usage(format!("give the path after {method}"))))?;
        let op = table
            .match_raw(method, &path)
            .map_err(|e| Stop::Ct(CtError::Usage(e.to_string())))?;
        let matches = raw_command(op)
            .try_get_matches_from(std::iter::once(format!("api {method}")).chain(words))
            .map_err(Stop::Clap)?;
        return Ok((op, matches, Some(path)));
    }
    let op = table.find(&first).ok_or_else(|| {
        Stop::Ct(CtError::Usage(format!(
            "no public operation is named {first}; `cloudthinker api --list` shows them"
        )))
    })?;
    let matches = verb_command(op, op.id.clone())
        .try_get_matches_from(std::iter::once(op.id.clone()).chain(words))
        .map_err(Stop::Clap)?;
    Ok((op, matches, None))
}

fn unknown() -> Stop {
    Stop::Ct(CtError::Usage(
        "unknown command; `cloudthinker --help` lists the commands".into(),
    ))
}

fn hand_word<'a>(cli: &Command, argv: &'a [String]) -> Option<&'a String> {
    argv.iter().find(|word| *word != cli.get_name())
}

fn hand_names(cli: &Command) -> impl Iterator<Item = &str> {
    cli.get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(Command::get_name)
}

fn hand_conflict(cli: &Command, argv: &[String]) -> Option<clap::Error> {
    let word = hand_word(cli, argv)?;
    hand_names(cli).any(|name| name == word).then(|| {
        cli.clone().error(
            clap::error::ErrorKind::ArgumentConflict,
            format!("the subcommand '{word}' cannot be used with the options given before it"),
        )
    })
}

fn hand_typo(cli: &Command, argv: &[String]) -> Option<clap::Error> {
    let word = hand_word(cli, argv)?;
    let (score, name) = hand_names(cli)
        .map(|name| (strsim::jaro(word, name), name))
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    (score > TYPO_SCORE).then(|| {
        cli.clone().error(
            clap::error::ErrorKind::InvalidSubcommand,
            format!(
                "unrecognized subcommand '{word}'\n\n  tip: a similar subcommand exists: '{name}'"
            ),
        )
    })
}

fn clap_exit(error: &clap::Error) -> ExitCode {
    if error.print().is_err() {
        return ExitCode::JobFailed;
    }
    match error.kind() {
        clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
            ExitCode::Ok
        }
        _ => ExitCode::Usage,
    }
}

pub async fn run_folded(
    base_url: &str,
    workspace: Option<&str>,
    yes: bool,
    operation: &str,
    args: Vec<String>,
) -> ExitCode {
    let argv = std::iter::once(operation.to_string()).chain(args).collect();
    run(base_url, workspace, yes, Target::Api(argv)).await
}

pub async fn list(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    let store_workspace = if env_token_is_set() { None } else { workspace };
    let client = match build_client(base_url, store_workspace) {
        Ok(client) => client,
        Err(err) => return exit::report(&err),
    };
    match client.public_spec().await {
        Ok(spec) => exit::finish_output(output::emit_public_operations(&spec.table, json)),
        Err(err) => exit::report(&err),
    }
}

fn command_tree(table: &OperationTable) -> Command {
    let mut root = Command::new("cloudthinker")
        .subcommand_required(true)
        .arg_required_else_help(true);
    let mut modules: Vec<&str> = table
        .operations
        .iter()
        .map(|op| op.cli.module.as_str())
        .collect();
    modules.sort_unstable();
    modules.dedup();
    for module in modules {
        let ops: Vec<&Operation> = table
            .operations
            .iter()
            .filter(|op| op.cli.module == module)
            .collect();
        let mut module_cmd = Command::new(module.to_string())
            .about(format!("CloudThinker {module} public API"))
            .subcommand_required(true)
            .arg_required_else_help(true);
        for op in ops.iter().filter(|op| op.cli.resource.is_none()) {
            module_cmd = module_cmd.subcommand(verb_command(op, op.cli.verb.clone()));
        }
        let mut resources: Vec<&str> = ops
            .iter()
            .filter_map(|op| op.cli.resource.as_deref())
            .collect();
        resources.sort_unstable();
        resources.dedup();
        for resource in resources {
            let mut resource_cmd = Command::new(resource.to_string())
                .about(format!("{module} {resource}"))
                .subcommand_required(true)
                .arg_required_else_help(true);
            for op in ops
                .iter()
                .filter(|op| op.cli.resource.as_deref() == Some(resource))
            {
                resource_cmd = resource_cmd.subcommand(verb_command(op, op.cli.verb.clone()));
            }
            module_cmd = module_cmd.subcommand(resource_cmd);
        }
        root = root.subcommand(module_cmd);
    }
    root
}

fn verb_command(op: &Operation, name: String) -> Command {
    let mut cmd = Command::new(name)
        .about(operation_summary(op))
        .after_help(format!(
            "Operation {} · {} {} · permission {}",
            op.id,
            op.method,
            op.path,
            op.permission.as_deref().unwrap_or("none")
        ));
    if op.workspace_scoped {
        cmd = cmd.arg(workspace_arg());
    }
    for param in &op.path_params {
        cmd = cmd.arg(
            Arg::new(param.name.clone())
                .value_name(param.name.to_ascii_uppercase())
                .required(true)
                .help(param_help(param, "ID")),
        );
    }
    for param in &op.query_params {
        cmd = cmd.arg(query_arg(param));
    }
    with_write_args(cmd, op)
}

fn raw_command(op: &Operation) -> Command {
    let cmd = Command::new("api")
        .about(operation_summary(op))
        .after_help(format!("Matched operation {}", op.id))
        .arg(
            Arg::new(ARG_QUERY)
                .long(ARG_QUERY)
                .value_name("NAME=VALUE")
                .action(ArgAction::Append)
                .help("A query parameter the operation declares; repeat for more"),
        );
    with_write_args(cmd, op)
}

fn with_write_args(mut cmd: Command, op: &Operation) -> Command {
    if op.paginated {
        cmd = cmd.arg(
            Arg::new(ARG_ALL)
                .long(ARG_ALL)
                .action(ArgAction::SetTrue)
                .help("Follow next_cursor and print every item (at most 10,000)"),
        );
    }
    if op.body.is_some() {
        cmd = cmd
            .arg(
                Arg::new(ARG_BODY)
                    .long(ARG_BODY)
                    .value_name("JSON|@FILE|-")
                    .help("Request body as JSON text, @path to a JSON file, or - for stdin"),
            )
            .arg(
                Arg::new(ARG_SET)
                    .long(ARG_SET)
                    .value_name("KEY=VALUE")
                    .action(ArgAction::Append)
                    .help("Set one top-level body field; the value is read as JSON when it parses, else as text"),
            );
    }
    if op.method.is_write() {
        cmd = cmd.arg(
            Arg::new(ARG_IDEMPOTENCY)
                .long(ARG_IDEMPOTENCY)
                .value_name("KEY")
                .help("Retry-safe key for this write; a fresh one is sent when omitted"),
        );
    }
    if op.method == Method::Patch || header(op, HEADER_IF_MATCH).is_some() {
        let required = header(op, HEADER_IF_MATCH).is_some_and(|p| p.required);
        cmd = cmd.arg(
            Arg::new(ARG_IF_MATCH)
                .long(ARG_IF_MATCH)
                .value_name("ETAG")
                .required(required)
                .help("Only write when the resource still has this ETag (from a previous get)"),
        );
    }
    cmd.arg(
        Arg::new(ARG_YES)
            .long(ARG_YES)
            .short('y')
            .action(ArgAction::SetTrue)
            .help("Run without asking when the operation asks first; required without a terminal"),
    )
}

fn workspace_arg() -> Arg {
    Arg::new(ARG_WORKSPACE)
        .short('w')
        .long(ARG_WORKSPACE)
        .value_name("WORKSPACE_ID")
        .value_parser(|value: &str| {
            if value.trim().is_empty() {
                Err("Pass a workspace ID or leave out --workspace.")
            } else {
                Ok(value.to_owned())
            }
        })
        .help("Workspace to call; defaults to the workspace of your login")
}

fn query_arg(param: &Param) -> Arg {
    let arg = Arg::new(param.name.clone())
        .long(param.flag.clone())
        .required(param.required)
        .help(param_help(param, "VALUE"));
    match &param.kind {
        ParamKind::String => arg.value_name("TEXT"),
        ParamKind::Integer { .. } => arg.value_name("N").value_parser(value_parser!(i64)),
        ParamKind::Number { .. } => arg.value_name("N").value_parser(value_parser!(f64)),
        ParamKind::Boolean => arg
            .value_name("true|false")
            .value_parser(value_parser!(bool)),
        ParamKind::Enum { values } => arg.value_parser(values.clone()),
        ParamKind::Array { items } => {
            let arg = arg.action(ArgAction::Append);
            match items.as_ref() {
                ParamKind::Enum { values } => arg.value_parser(values.clone()),
                _ => arg.value_name("VALUE"),
            }
        }
    }
}

fn param_help(param: &Param, fallback: &str) -> String {
    match (&param.description, &param.kind) {
        (Some(text), _) => text.clone(),
        (
            None,
            ParamKind::Integer {
                minimum: Some(min),
                maximum: Some(max),
            },
        ) => {
            format!("{} ({min}-{max})", param.name.replace('_', " "))
        }
        (None, ParamKind::Array { .. }) => format!("{} (repeatable)", param.name.replace('_', " ")),
        (None, _) => format!("{} {fallback}", param.name.replace('_', " ")),
    }
}

fn header<'a>(op: &'a Operation, name: &str) -> Option<&'a Param> {
    op.header_params
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}

fn build_call(
    client: &CtClient,
    op: &Operation,
    matches: &ArgMatches,
    raw_path: Option<String>,
    workspace: Option<&str>,
) -> Result<PublicCall, CtError> {
    let path = match raw_path {
        Some(path) => path,
        None => {
            let workspace_id = match op.workspace_scoped {
                true => Some(client.public_workspace(workspace)?.to_string()),
                false => None,
            };
            let values: Vec<&str> = op
                .path_params
                .iter()
                .filter_map(|p| matches.get_one::<String>(&p.name).map(String::as_str))
                .collect();
            op.fill_path(workspace_id.as_deref(), &values)
                .map_err(|e| CtError::Usage(e.to_string()))?
        }
    };
    let query = match matches.try_get_many::<String>(ARG_QUERY) {
        Ok(Some(raw)) => raw_query(op, raw)?,
        Ok(None) => Vec::new(),
        Err(_) => typed_query(op, matches),
    };
    let body = BodyInput {
        raw: match matches.try_get_one::<String>(ARG_BODY).ok().flatten() {
            Some(value) => Some(read_body(value)?),
            None => None,
        },
        fields: match matches.try_get_many::<String>(ARG_SET).ok().flatten() {
            Some(values) => values
                .map(|v| split_pair(ARG_SET, v))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        }
        .into_iter()
        .map(|(key, value)| BodyField { key, value })
        .collect(),
    };
    if op.body.as_ref().is_some_and(|b| b.required) && body.raw.is_none() && body.fields.is_empty()
    {
        return Err(CtError::Usage(format!("{} needs --body or --set", op.id)));
    }
    Ok(PublicCall {
        method: op.method,
        path,
        query,
        body,
        idempotency_key: optional(matches, ARG_IDEMPOTENCY),
        if_match: optional(matches, ARG_IF_MATCH),
        all_pages: matches
            .try_get_one::<bool>(ARG_ALL)
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false),
    })
}

fn optional(matches: &ArgMatches, id: &str) -> Option<String> {
    matches.try_get_one::<String>(id).ok().flatten().cloned()
}

fn typed_query(op: &Operation, matches: &ArgMatches) -> Vec<QueryValue> {
    let mut query = Vec::new();
    for param in &op.query_params {
        let values: Vec<String> = match &param.kind {
            ParamKind::Integer { .. } => matches
                .get_many::<i64>(&param.name)
                .into_iter()
                .flatten()
                .map(i64::to_string)
                .collect(),
            ParamKind::Number { .. } => matches
                .get_many::<f64>(&param.name)
                .into_iter()
                .flatten()
                .map(f64::to_string)
                .collect(),
            ParamKind::Boolean => matches
                .get_many::<bool>(&param.name)
                .into_iter()
                .flatten()
                .map(bool::to_string)
                .collect(),
            ParamKind::String | ParamKind::Enum { .. } | ParamKind::Array { .. } => matches
                .get_many::<String>(&param.name)
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
        };
        query.extend(values.into_iter().map(|value| QueryValue {
            name: param.name.clone(),
            value,
        }));
    }
    query
}

fn raw_query<'a>(
    op: &Operation,
    raw: impl Iterator<Item = &'a String>,
) -> Result<Vec<QueryValue>, CtError> {
    let mut query = Vec::new();
    for pair in raw {
        let (name, value) = split_pair(ARG_QUERY, pair)?;
        let Some(param) = op.query_params.iter().find(|p| p.name == name) else {
            return Err(CtError::Usage(format!(
                "{} has no query parameter {name}",
                op.id
            )));
        };
        if query.iter().any(|q: &QueryValue| q.name == name)
            && !matches!(param.kind, ParamKind::Array { .. })
        {
            return Err(CtError::Usage(format!("{name} takes one value")));
        }
        query.push(QueryValue { name, value });
    }
    Ok(query)
}

fn split_pair(flag: &str, pair: &str) -> Result<(String, String), CtError> {
    pair.split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .ok_or_else(|| CtError::Usage(format!("--{flag} takes NAME=VALUE, got '{pair}'")))
}

fn read_body(value: &str) -> Result<String, CtError> {
    let read = |reader: &mut dyn Read, source: &str| {
        let mut text = String::new();
        reader
            .take(BODY_MAX_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|e| CtError::Usage(format!("could not read the body from {source}: {e}")))?;
        if text.len() as u64 > BODY_MAX_BYTES {
            return Err(CtError::Usage(format!(
                "the body is larger than {BODY_MAX_BYTES} bytes"
            )));
        }
        Ok(text)
    };
    match value {
        "-" => read(&mut std::io::stdin().lock(), "stdin"),
        path if path.starts_with('@') => {
            let path = &path[1..];
            let mut file = std::fs::File::open(path)
                .map_err(|e| CtError::Usage(format!("could not open {path}: {e}")))?;
            read(&mut file, path)
        }
        text => Ok(text.to_string()),
    }
}

async fn send(client: &CtClient, call: &PublicCall) -> ExitCode {
    match client.public_call(call).await {
        Ok(reply) => {
            exit::finish_output(output::emit_public_body(&reply.body, reply.etag.as_deref()))
        }
        Err(err) => exit::report(&err),
    }
}

pub fn annotate_help(mut root: Command, base_url: &str) -> Command {
    let Some(table) = cloudthinker_client::cached_public_spec(base_url) else {
        return root.after_long_help(
            "Public API commands (cloudthinker <module> <resource> <verb>) load from the server; run `cloudthinker api --list` to see them.",
        );
    };
    let mut modules: Vec<&str> = table
        .operations
        .iter()
        .map(|op| op.cli.module.as_str())
        .collect();
    modules.sort_unstable();
    modules.dedup();
    for module in &modules {
        let mut resources: Vec<&str> = table
            .operations
            .iter()
            .filter(|op| op.cli.module == *module)
            .map(|op| op.cli.group())
            .collect();
        resources.sort_unstable();
        resources.dedup();
        let line = format!(
            "Public API resources: {} (cloudthinker {module} <resource> --help)",
            resources.join(", ")
        );
        if root.find_subcommand(module).is_some() {
            root = root.mut_subcommand(module, |cmd| {
                let text = match cmd.get_after_help() {
                    Some(existing) => format!("{existing}\n\n{line}"),
                    None => line.clone(),
                };
                cmd.after_help(text)
            });
        }
    }
    root.after_long_help(format!(
        "Public API modules: {}. Run cloudthinker <module> --help, or `cloudthinker api --list` for every command.",
        modules.join(", ")
    ))
}

pub fn check(root: &Command, path: &std::path::Path) -> ExitCode {
    let table = match cloudthinker_client::read_public_spec_file(path) {
        Ok(table) => table,
        Err(err) => return exit::report(&err),
    };
    let mut root = root.clone();
    root.build();
    let collisions = collisions(&root, &table);
    for collision in &collisions {
        output::eprintln_error(collision);
    }
    if collisions.is_empty() {
        println!(
            "{} public operations, no command collisions",
            table.operations.len()
        );
        ExitCode::Ok
    } else {
        ExitCode::JobFailed
    }
}

fn collisions(root: &Command, table: &OperationTable) -> Vec<String> {
    let mut found = Vec::new();
    for op in &table.operations {
        let Some(module) = root.find_subcommand(&op.cli.module) else {
            continue;
        };
        if !module.is_allow_external_subcommands_set() {
            found.push(format!(
                "{}: `cloudthinker {}` is a hand-made command that does not take public API resources",
                op.id, op.cli.module
            ));
        } else if module.find_subcommand(op.cli.group()).is_some()
            && !FOLDED.contains(&op.id.as_str())
        {
            found.push(format!(
                "{}: `cloudthinker {} {}` is already a hand-made command; rename one of them",
                op.id,
                op.cli.module,
                op.cli.group()
            ));
        }
    }
    found.sort();
    found.dedup();
    found
}
