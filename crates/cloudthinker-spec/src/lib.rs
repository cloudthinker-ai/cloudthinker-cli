use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const WORKSPACE_PREFIX: &str = "/v1/workspaces/{workspace_id}/";
const WORKSPACE_PARAM: &str = "workspace_id";
const WORKSPACE_TEMPLATE: &str = "{workspace_id}";
const MAX_REF_DEPTH: u8 = 8;
const MAX_PATH_BYTES: usize = 2048;
const MAX_SEGMENT_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("the public spec is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the public spec has {} problem(s); first: {}", .0.len(), .0.first().map(ToString::to_string).unwrap_or_default())]
    Invalid(Vec<Issue>),
}

pub fn parse(bytes: &[u8]) -> Result<OperationTable, SpecError> {
    let spec: Spec = serde_json::from_slice(bytes)?;
    build_table(&spec).map_err(SpecError::Invalid)
}

// --- spec subset ------------------------------------------------------------

#[derive(Deserialize)]
struct Spec {
    #[serde(default)]
    servers: Vec<Server>,
    paths: BTreeMap<String, PathItem>,
    #[serde(default)]
    components: Components,
}

#[derive(Deserialize)]
struct Server {
    url: String,
}

#[derive(Deserialize, Default)]
struct Components {
    #[serde(default)]
    schemas: BTreeMap<String, Schema>,
    #[serde(default)]
    parameters: BTreeMap<String, RawParam>,
    #[serde(default)]
    responses: BTreeMap<String, RawResponse>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ParamOrRef {
    Ref {
        #[serde(rename = "$ref")]
        reference: String,
    },
    Inline(Box<RawParam>),
}

#[derive(Deserialize)]
struct PathItem {
    #[serde(default)]
    parameters: Vec<ParamOrRef>,
    get: Option<RawOperation>,
    post: Option<RawOperation>,
    patch: Option<RawOperation>,
    put: Option<RawOperation>,
    delete: Option<RawOperation>,
    head: Option<RawOperation>,
    options: Option<RawOperation>,
    trace: Option<RawOperation>,
}

#[derive(Deserialize)]
struct RawOperation {
    #[serde(rename = "operationId")]
    operation_id: Option<String>,
    summary: Option<String>,
    #[serde(default)]
    deprecated: bool,
    #[serde(default)]
    parameters: Vec<ParamOrRef>,
    #[serde(rename = "requestBody")]
    request_body: Option<RawBody>,
    #[serde(default)]
    responses: BTreeMap<String, RawResponse>,
    #[serde(rename = "x-scope")]
    scope: Option<String>,
    #[serde(rename = "x-confirm", default)]
    confirm: bool,
    #[serde(rename = "x-audience")]
    audience: Option<String>,
}

#[derive(Deserialize)]
struct RawParam {
    name: String,
    #[serde(rename = "in")]
    location: Location,
    #[serde(default)]
    required: bool,
    description: Option<String>,
    schema: Option<Schema>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Location {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Deserialize)]
struct RawBody {
    #[serde(default)]
    required: bool,
    #[serde(default)]
    content: BTreeMap<String, MediaType>,
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(rename = "$ref")]
    reference: Option<String>,
    #[serde(default)]
    content: BTreeMap<String, MediaType>,
}

#[derive(Deserialize)]
struct MediaType {
    schema: Option<Schema>,
}

#[derive(Deserialize, Default)]
struct Schema {
    #[serde(rename = "$ref")]
    reference: Option<String>,
    #[serde(rename = "type")]
    kind: Option<TypeField>,
    format: Option<String>,
    #[serde(rename = "enum")]
    values: Option<Vec<Scalar>>,
    items: Option<Box<Schema>>,
    #[serde(rename = "anyOf", default)]
    any_of: Vec<Schema>,
    #[serde(rename = "oneOf", default)]
    one_of: Vec<Schema>,
    #[serde(rename = "allOf", default)]
    all_of: Vec<Schema>,
    #[serde(default)]
    properties: BTreeMap<String, Schema>,
    minimum: Option<f64>,
    maximum: Option<f64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TypeField {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Str(value) => f.write_str(value),
        }
    }
}

// --- the table --------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationTable {
    pub base_path: String,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub module: String,
    pub resource: Option<String>,
    pub verb: String,
    pub cli: CliName,
    pub method: Method,
    pub path: String,
    pub workspace_scoped: bool,
    pub path_params: Vec<Param>,
    pub query_params: Vec<Param>,
    pub header_params: Vec<Param>,
    pub body: Option<Body>,
    pub response: Option<String>,
    pub paginated: bool,
    pub scope: Option<String>,
    pub confirm: bool,
    pub deprecated: bool,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliName {
    pub module: String,
    pub resource: Option<String>,
    pub verb: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub flag: String,
    pub required: bool,
    pub kind: ParamKind,
    pub format: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    String,
    Integer {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Boolean,
    Enum {
        values: Vec<String>,
    },
    Array {
        items: Box<ParamKind>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Body {
    pub schema: Option<String>,
    pub required: bool,
}

// --- problems ---------------------------------------------------------------

#[derive(Debug)]
pub struct Issue {
    pub at: String,
    pub problem: Problem,
}

#[derive(Debug)]
pub enum Problem {
    MissingOperationId,
    BadOperationId,
    DuplicateOperationId(String),
    MethodNotPublic(&'static str),
    PathOutsideWorkspace,
    WorkspacelessNotRead,
    ModuleMismatch { path_module: String },
    ResourceMismatch { path_resource: String },
    UndeclaredPathParam(String),
    PathParamNotInPath(String),
    UnsupportedParam(String),
    UnresolvedRef(String),
    MissingExtension(&'static str),
    NotPublicAudience,
    BodyNotJson,
    DuplicateCliName(String),
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.at)?;
        match &self.problem {
            Problem::MissingOperationId => f.write_str("operationId is missing"),
            Problem::BadOperationId => {
                f.write_str("operationId must be module.resource.verb in snake_case (token.get is the one module.verb)")
            }
            Problem::DuplicateOperationId(other) => {
                write!(f, "operationId is also used by {other}")
            }
            Problem::MethodNotPublic(method) => {
                write!(f, "{method} is not a public method (GET, POST, PATCH, DELETE only)")
            }
            Problem::PathOutsideWorkspace => write!(
                f,
                "a path with {{{WORKSPACE_PARAM}}} must start with {WORKSPACE_PREFIX}"
            ),
            Problem::WorkspacelessNotRead => {
                write!(f, "an operation without {{{WORKSPACE_PARAM}}} must be a GET with no x-scope")
            }
            Problem::ModuleMismatch { path_module } => {
                write!(
                    f,
                    "operationId module differs from path module {path_module}"
                )
            }
            Problem::ResourceMismatch { path_resource } => {
                write!(
                    f,
                    "operationId resource differs from path resource {path_resource}"
                )
            }
            Problem::UndeclaredPathParam(name) => {
                write!(f, "path parameter {{{name}}} is not declared")
            }
            Problem::PathParamNotInPath(name) => {
                write!(f, "path parameter {name} does not appear in the path")
            }
            Problem::UnsupportedParam(name) => {
                write!(
                    f,
                    "parameter {name} needs a scalar, enum or scalar-array schema"
                )
            }
            Problem::UnresolvedRef(reference) => write!(f, "unresolved $ref {reference}"),
            Problem::MissingExtension(name) => write!(f, "{name} is missing"),
            Problem::NotPublicAudience => f.write_str("x-audience must be public"),
            Problem::BodyNotJson => f.write_str("request body must be application/json"),
            Problem::DuplicateCliName(other) => {
                write!(f, "CLI command name is also used by {other}")
            }
        }
    }
}

// --- building ---------------------------------------------------------------

fn build_table(spec: &Spec) -> Result<OperationTable, Vec<Issue>> {
    let mut issues = Vec::new();
    let mut operations = Vec::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut cli_names: BTreeMap<String, String> = BTreeMap::new();

    for (path, item) in &spec.paths {
        for (method, raw) in item.operations() {
            let at = format!("{method} {path}");
            let Some(method) = public_method(method, &at, &mut issues) else {
                continue;
            };
            let op = match build_operation(spec, path, item, raw, method) {
                Ok(op) => op,
                Err(problems) => {
                    issues.extend(problems.into_iter().map(|problem| Issue {
                        at: at.clone(),
                        problem,
                    }));
                    continue;
                }
            };
            if let Some(other) = ids.insert(op.id.clone(), at.clone()) {
                issues.push(Issue {
                    at: at.clone(),
                    problem: Problem::DuplicateOperationId(other),
                });
            }
            let cli = op.cli.words().join(" ");
            if let Some(other) = cli_names.insert(cli, op.id.clone()) {
                issues.push(Issue {
                    at,
                    problem: Problem::DuplicateCliName(other),
                });
            }
            operations.push(op);
        }
    }

    if !issues.is_empty() {
        return Err(issues);
    }
    operations.sort_by(|a, b| a.id.cmp(&b.id));
    let base_path = spec
        .servers
        .first()
        .map(|s| s.url.clone())
        .unwrap_or_default();
    Ok(OperationTable {
        base_path,
        operations,
    })
}

impl PathItem {
    fn operations(&self) -> impl Iterator<Item = (&'static str, &RawOperation)> {
        [
            ("GET", self.get.as_ref()),
            ("POST", self.post.as_ref()),
            ("PATCH", self.patch.as_ref()),
            ("PUT", self.put.as_ref()),
            ("DELETE", self.delete.as_ref()),
            ("HEAD", self.head.as_ref()),
            ("OPTIONS", self.options.as_ref()),
            ("TRACE", self.trace.as_ref()),
        ]
        .into_iter()
        .filter_map(|(method, op)| op.map(|op| (method, op)))
    }
}

fn public_method(method: &'static str, at: &str, issues: &mut Vec<Issue>) -> Option<Method> {
    match method {
        "GET" => Some(Method::Get),
        "POST" => Some(Method::Post),
        "PATCH" => Some(Method::Patch),
        "DELETE" => Some(Method::Delete),
        other => {
            issues.push(Issue {
                at: at.to_string(),
                problem: Problem::MethodNotPublic(other),
            });
            None
        }
    }
}

fn build_operation(
    spec: &Spec,
    path: &str,
    item: &PathItem,
    raw: &RawOperation,
    method: Method,
) -> Result<Operation, Vec<Problem>> {
    let mut problems = Vec::new();
    let mut push = |problem| problems.push(problem);

    let Some(id) = raw.operation_id.as_deref() else {
        return Err(vec![Problem::MissingOperationId]);
    };
    let workspace_scoped = template_params(path).any(|name| name == WORKSPACE_PARAM);
    let Some((module, resource, verb)) = split_operation_id(id, workspace_scoped) else {
        return Err(vec![Problem::BadOperationId]);
    };
    match path.strip_prefix(WORKSPACE_PREFIX) {
        None if !workspace_scoped => {
            if method != Method::Get || raw.scope.is_some() {
                push(Problem::WorkspacelessNotRead);
            }
        }
        None => push(Problem::PathOutsideWorkspace),
        Some(rest) => {
            let mut segments = rest.split('/');
            let path_module = segments.next().unwrap_or_default();
            let path_resource = segments.next().unwrap_or_default();
            if path_module != module {
                push(Problem::ModuleMismatch {
                    path_module: path_module.to_string(),
                });
            }
            if Some(path_resource.replace('-', "_").as_str()) != resource {
                push(Problem::ResourceMismatch {
                    path_resource: path_resource.to_string(),
                });
            }
        }
    }

    if workspace_scoped && raw.scope.as_deref().is_none_or(str::is_empty) {
        push(Problem::MissingExtension("x-scope"));
    }
    if raw.audience.as_deref() != Some("public") {
        push(Problem::NotPublicAudience);
    }

    let mut declared = Vec::new();
    for entry in item.parameters.iter().chain(raw.parameters.iter()) {
        match entry {
            ParamOrRef::Inline(param) => declared.push(param.as_ref()),
            ParamOrRef::Ref { reference } => match reference
                .strip_prefix("#/components/parameters/")
                .and_then(|name| spec.components.parameters.get(name))
            {
                Some(param) => declared.push(param),
                None => push(Problem::UnresolvedRef(reference.clone())),
            },
        }
    }
    let mut path_params = Vec::new();
    let mut query_params = Vec::new();
    let mut header_params = Vec::new();
    let mut declared_path = BTreeSet::new();
    for param in declared {
        let kind = match param
            .schema
            .as_ref()
            .map(|s| param_kind(spec, s, MAX_REF_DEPTH))
        {
            Some(Ok(kind)) => kind,
            Some(Err(reference)) => {
                push(Problem::UnresolvedRef(reference));
                continue;
            }
            None => ParamKind::String,
        };
        let built = Param {
            flag: param.name.replace('_', "-").to_ascii_lowercase(),
            name: param.name.clone(),
            required: param.required || param.location == Location::Path,
            kind,
            format: param.schema.as_ref().and_then(|s| s.format.clone()),
            description: param.description.clone(),
        };
        match (param.location, &built.kind) {
            (Location::Path, ParamKind::Array { .. })
            | (Location::Header, ParamKind::Array { .. }) => {
                push(Problem::UnsupportedParam(param.name.clone()));
            }
            (Location::Path, _) => {
                declared_path.insert(param.name.clone());
                if param.name != WORKSPACE_PARAM {
                    path_params.push(built);
                }
            }
            (Location::Query, _) => query_params.push(built),
            (Location::Header, _) => header_params.push(built),
            (Location::Cookie, _) => push(Problem::UnsupportedParam(param.name.clone())),
        }
    }
    for name in template_params(path) {
        if !declared_path.contains(name) {
            push(Problem::UndeclaredPathParam(name.to_string()));
        }
    }
    let in_path: BTreeSet<&str> = template_params(path).collect();
    for name in &declared_path {
        if !in_path.contains(name.as_str()) {
            push(Problem::PathParamNotInPath(name.clone()));
        }
    }
    let order: Vec<&str> = template_params(path).collect();
    path_params.sort_by_key(|p| order.iter().position(|n| *n == p.name));

    let body = match &raw.request_body {
        None => None,
        Some(body) => match body.content.get("application/json") {
            Some(media) => Some(Body {
                schema: media.schema.as_ref().and_then(schema_name),
                required: body.required,
            }),
            None => {
                push(Problem::BodyNotJson);
                None
            }
        },
    };

    let response_schema = success_schema(spec, raw);
    let paginated = query_params.iter().any(|p| p.name == "cursor")
        && response_schema
            .map(|schema| is_cursor_page(spec, schema, MAX_REF_DEPTH))
            .unwrap_or(false);

    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Operation {
        id: id.to_string(),
        cli: CliName {
            module: module.replace('_', "-"),
            resource: resource.map(|r| r.replace('_', "-")),
            verb: verb.replace('_', "-"),
        },
        module: module.to_string(),
        resource: resource.map(str::to_string),
        verb: verb.to_string(),
        method,
        path: path.to_string(),
        workspace_scoped,
        path_params,
        query_params,
        header_params,
        body,
        response: response_schema.and_then(schema_name),
        paginated,
        scope: raw.scope.clone(),
        confirm: raw.confirm,
        deprecated: raw.deprecated,
        summary: raw.summary.clone(),
    })
}

fn split_operation_id(id: &str, workspace_scoped: bool) -> Option<(&str, Option<&str>, &str)> {
    let parts: Vec<&str> = id.split('.').collect();
    if !parts.iter().all(|segment| is_snake_segment(segment)) {
        return None;
    }
    match (parts.as_slice(), workspace_scoped) {
        ([module, resource, verb], true) => Some((module, Some(resource), verb)),
        ([module, verb], false) => Some((module, None, verb)),
        _ => None,
    }
}

fn is_snake_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !segment.ends_with('_')
        && !segment.contains("__")
}

fn template_params(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
}

fn schema_name(schema: &Schema) -> Option<String> {
    schema
        .reference
        .as_deref()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .map(str::to_string)
}

fn resolve<'a>(spec: &'a Spec, schema: &'a Schema) -> Result<&'a Schema, String> {
    match schema.reference.as_deref() {
        None => Ok(schema),
        Some(reference) => reference
            .strip_prefix("#/components/schemas/")
            .and_then(|name| spec.components.schemas.get(name))
            .ok_or_else(|| reference.to_string()),
    }
}

fn param_kind(spec: &Spec, schema: &Schema, depth: u8) -> Result<ParamKind, String> {
    let unsupported = || {
        Err(schema
            .reference
            .clone()
            .unwrap_or_else(|| "inline schema".into()))
    };
    if depth == 0 {
        return unsupported();
    }
    let schema = resolve(spec, schema)?;
    if schema.reference.is_some() {
        return param_kind(spec, schema, depth - 1);
    }
    let variants: Vec<&Schema> = schema
        .any_of
        .iter()
        .chain(&schema.one_of)
        .chain(&schema.all_of)
        .filter(|variant| !variant.is_null())
        .collect();
    if let [only] = variants.as_slice() {
        return param_kind(spec, only, depth - 1);
    }
    if !variants.is_empty() {
        return unsupported();
    }
    if let Some(values) = &schema.values {
        return Ok(ParamKind::Enum {
            values: values.iter().map(Scalar::to_string).collect(),
        });
    }
    match schema.primary_type() {
        Some("string") => Ok(ParamKind::String),
        Some("integer") => Ok(ParamKind::Integer {
            minimum: schema.minimum,
            maximum: schema.maximum,
        }),
        Some("number") => Ok(ParamKind::Number {
            minimum: schema.minimum,
            maximum: schema.maximum,
        }),
        Some("boolean") => Ok(ParamKind::Boolean),
        Some("array") => match schema
            .items
            .as_deref()
            .map(|i| param_kind(spec, i, depth - 1))
        {
            Some(Ok(ParamKind::Array { .. })) | None => unsupported(),
            Some(Ok(items)) => Ok(ParamKind::Array {
                items: Box::new(items),
            }),
            Some(Err(reference)) => Err(reference),
        },
        _ => unsupported(),
    }
}

impl Schema {
    fn primary_type(&self) -> Option<&str> {
        match self.kind.as_ref()? {
            TypeField::One(kind) => Some(kind),
            TypeField::Many(kinds) => {
                let mut real = kinds.iter().filter(|k| k.as_str() != "null");
                match (real.next(), real.next()) {
                    (Some(kind), None) => Some(kind),
                    _ => None,
                }
            }
        }
    }

    fn is_null(&self) -> bool {
        matches!(&self.kind, Some(TypeField::One(kind)) if kind == "null")
    }
}

fn success_schema<'a>(spec: &'a Spec, raw: &'a RawOperation) -> Option<&'a Schema> {
    raw.responses
        .iter()
        .filter(|(status, _)| status.starts_with('2'))
        .filter_map(|(_, response)| match response.reference.as_deref() {
            None => Some(response),
            Some(reference) => reference
                .strip_prefix("#/components/responses/")
                .and_then(|name| spec.components.responses.get(name)),
        })
        .find_map(|response| response.content.get("application/json"))
        .and_then(|media| media.schema.as_ref())
}

fn is_cursor_page(spec: &Spec, schema: &Schema, depth: u8) -> bool {
    if depth == 0 {
        return false;
    }
    match resolve(spec, schema) {
        Ok(found) if found.reference.is_some() => is_cursor_page(spec, found, depth - 1),
        Ok(found) => {
            found.properties.contains_key("items") && found.properties.contains_key("next_cursor")
        }
        Err(_) => false,
    }
}

// --- calling ----------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("the workspace must be a UUID")]
    Workspace,
    #[error("{name} must be 1-128 letters, digits, '-' or '_'")]
    Segment { name: String },
    #[error("expected {expected} path value(s), got {got}")]
    Arity { expected: usize, got: usize },
    #[error(
        "a raw path must start with /v1/, stay ASCII and under 2 KB, and carry no '..', '//', '\\', '%', '?' or '#'"
    )]
    Unsafe,
    #[error("no public {method} operation matches {path}")]
    NoMatch { method: Method, path: String },
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PATCH" => Some(Self::Patch),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn is_write(self) -> bool {
        self != Self::Get
    }
}

impl CliName {
    pub fn words(&self) -> Vec<&str> {
        std::iter::once(self.module.as_str())
            .chain(self.resource.as_deref())
            .chain(std::iter::once(self.verb.as_str()))
            .collect()
    }

    pub fn group(&self) -> &str {
        self.resource.as_deref().unwrap_or(&self.verb)
    }
}

impl Operation {
    pub fn fill_path(
        &self,
        workspace_id: Option<&str>,
        values: &[&str],
    ) -> Result<String, PathError> {
        let workspace_id = match (self.workspace_scoped, workspace_id) {
            (false, _) => "",
            (true, Some(id)) if uuid::Uuid::parse_str(id).is_ok() => id,
            (true, _) => return Err(PathError::Workspace),
        };
        if values.len() != self.path_params.len() {
            return Err(PathError::Arity {
                expected: self.path_params.len(),
                got: values.len(),
            });
        }
        let mut given = self.path_params.iter().zip(values);
        let mut filled = Vec::new();
        for segment in self.path.split('/') {
            match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(WORKSPACE_PARAM) => filled.push(workspace_id),
                Some(_) => {
                    let Some((param, value)) = given.next() else {
                        return Err(PathError::Arity {
                            expected: self.path_params.len(),
                            got: values.len(),
                        });
                    };
                    if !is_safe_segment(value) {
                        return Err(PathError::Segment {
                            name: param.name.clone(),
                        });
                    }
                    filled.push(value);
                }
                None => filled.push(segment),
            }
        }
        Ok(filled.join("/"))
    }

    pub fn header(&self, name: &str) -> Option<&Param> {
        self.header_params
            .iter()
            .find(|param| param.name.eq_ignore_ascii_case(name))
    }

    pub fn workspace_id(&self, path: &str) -> Option<uuid::Uuid> {
        self.path
            .split('/')
            .zip(path.split('/'))
            .find(|(want, _)| *want == WORKSPACE_TEMPLATE)
            .and_then(|(_, got)| uuid::Uuid::parse_str(got).ok())
    }

    pub fn canonical_path(&self, path: &str) -> String {
        self.path
            .split('/')
            .zip(path.split('/'))
            .map(|(want, got)| {
                let name = want.strip_prefix('{').and_then(|s| s.strip_suffix('}'));
                let uuid = name.is_some_and(|name| {
                    name == WORKSPACE_PARAM
                        || self.path_params.iter().any(|param| {
                            param.name == name && param.format.as_deref() == Some("uuid")
                        })
                });
                match uuid::Uuid::parse_str(got) {
                    Ok(id) if uuid => id.hyphenated().to_string(),
                    _ => got.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    fn matches(&self, method: Method, path: &str) -> bool {
        if self.method != method {
            return false;
        }
        let template: Vec<&str> = self.path.split('/').collect();
        let actual: Vec<&str> = path.split('/').collect();
        template.len() == actual.len()
            && template.iter().zip(&actual).all(|(want, got)| {
                match want.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    Some(WORKSPACE_PARAM) => uuid::Uuid::parse_str(got).is_ok(),
                    Some(_) => is_safe_segment(got),
                    None => want == got,
                }
            })
    }
}

impl OperationTable {
    pub fn find(&self, id: &str) -> Option<&Operation> {
        self.operations.iter().find(|op| op.id == id)
    }

    pub fn match_raw(&self, method: Method, path: &str) -> Result<&Operation, PathError> {
        check_raw_path(path)?;
        self.operations
            .iter()
            .find(|op| op.matches(method, path))
            .ok_or_else(|| PathError::NoMatch {
                method,
                path: path.to_string(),
            })
    }
}

fn check_raw_path(path: &str) -> Result<(), PathError> {
    let safe = path.len() <= MAX_PATH_BYTES
        && path.starts_with("/v1/")
        && path.bytes().all(|b| b.is_ascii_graphic())
        && !path.contains("//")
        && !path.contains("..")
        && !path.contains(['\\', '%', '?', '#']);
    if safe { Ok(()) } else { Err(PathError::Unsafe) }
}

fn is_safe_segment(value: &str) -> bool {
    (1..=MAX_SEGMENT_BYTES).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
