use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use cloudthinker_client::worker_types as api;
use serde_json::{Value, json};

pub const MAX_BYTES: usize = 1_400_000;
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARTIFACT_FILES: usize = 64;
const MAX_ENTRIES: usize = 10000;
const MAX_SEARCH_BYTES: usize = 16 * 1024 * 1024;

pub fn relative(path: &str) -> Result<&Path, &'static str> {
    let result = Path::new(path);
    if path.len() > 1024
        || result.components().count() > 64
        || path.contains('\0')
        || result.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("TRUSTED_ROOT_REJECTED");
    }
    Ok(if path.is_empty() {
        Path::new(".")
    } else {
        result
    })
}

pub fn read(dir: &Dir, path: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let bytes = read_prefix(dir, path, limit.min(MAX_BYTES) + 1)?;
    if bytes.len() > limit.min(MAX_BYTES) {
        return Err("FILE_TOO_LARGE");
    }
    Ok(bytes)
}

fn read_prefix(dir: &Dir, path: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed());
    let file = dir.open_with(relative(path)?, &options).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err("FILE_NOT_REGULAR");
    }
    let mut bytes = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    Ok(bytes)
}

fn write(dir: &Dir, path: &str, content: &[u8]) -> Result<(), &'static str> {
    if content.len() > MAX_BYTES {
        return Err("FILE_TOO_LARGE");
    }
    let path = relative(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        dir.create_dir_all(parent).map_err(io_error)?;
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed());
    let mut file = dir.open_with(path, &options).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err("FILE_NOT_REGULAR");
    }
    file.write_all(content).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

pub fn content(dir: &Dir, request: &api::FileContent) -> Result<Value, &'static str> {
    let bytes = read(
        dir,
        &request.path,
        request
            .max_bytes
            .and_then(|x| usize::try_from(x).ok())
            .unwrap_or(MAX_BYTES),
    )?;
    let (content, binary) = match String::from_utf8(bytes) {
        Ok(text) => (text, false),
        Err(error) => (
            base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            true,
        ),
    };
    let mut value = json!({"path": request.path, "name": Path::new(&request.path).file_name().and_then(|s| s.to_str()).unwrap_or("file"), "is_binary": binary});
    value["content"] = Value::String(content);
    Ok(value)
}

pub fn list(dir: &Dir, request: &api::FilesList) -> Result<Value, &'static str> {
    let path = request.path.as_deref().unwrap_or(".");
    let root = relative(path)?;
    let scan = scan(
        dir,
        root,
        request.search.as_ref().is_some_and(|s| !s.is_empty()),
    )?;
    let search = request.search.as_deref().unwrap_or("").to_lowercase();
    let matching: Vec<_> = scan
        .iter()
        .filter(|item| item.path.to_string_lossy().to_lowercase().contains(&search))
        .collect();
    let page = request.page.unwrap_or(1).max(1) as usize;
    let limit = request.limit.unwrap_or(100).clamp(1, 200) as usize;
    let offset = page.saturating_sub(1).saturating_mul(limit);
    let files: Vec<_> = matching.iter().skip(offset).take(limit).map(|entry| {
        let path = entry.path.to_string_lossy();
        json!({"id": path, "path": path, "name": entry.path.file_name().and_then(|s| s.to_str()).unwrap_or("file"), "type": if entry.directory {"directory"} else {"file"}, "size": entry.size, "modified_at": entry.modified})
    }).collect();
    Ok(
        json!({"files":files,"current_path":path,"page":page,"limit":limit,"total":matching.len(),"has_more":offset.saturating_add(limit)<matching.len(),"truncated":scan.len()==MAX_ENTRIES}),
    )
}

pub fn artifact_paths(dir: &Dir) -> Result<Vec<PathBuf>, &'static str> {
    let entries = match scan(dir, Path::new("output"), true) {
        Ok(entries) => entries,
        Err("FILE_NOT_FOUND") => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    let mut size = 0u64;
    if entries.len() == MAX_ENTRIES {
        return Err("EXECUTOR_ARTIFACT_LIMIT_EXCEEDED");
    }
    for entry in entries.into_iter().filter(|e| !e.directory) {
        size = size.saturating_add(entry.size);
        if paths.len() == MAX_ARTIFACT_FILES
            || size > MAX_ARTIFACT_BYTES as u64
            || entry.size > MAX_BYTES as u64
        {
            return Err("EXECUTOR_ARTIFACT_LIMIT_EXCEEDED");
        }
        paths.push(entry.path);
    }
    Ok(paths)
}

pub fn deliverables(dir: &Dir) -> Result<Value, &'static str> {
    let entries = match scan(dir, Path::new("output"), true) {
        Ok(entries) => entries,
        Err("FILE_NOT_FOUND") => return Ok(json!({"files":[],"current_path":"/"})),
        Err(error) => return Err(error),
    };
    let truncated = entries.len() == MAX_ENTRIES;
    let mut children: std::collections::BTreeMap<PathBuf, Vec<Value>> =
        std::collections::BTreeMap::new();
    for entry in entries.into_iter().rev() {
        let mut nested = children.remove(&entry.path).unwrap_or_default();
        nested.reverse();
        let parent = entry.path.parent().ok_or("TRUSTED_ROOT_REJECTED")?;
        let node = json!({
            "id":entry.path,"path":entry.path,
            "name":entry.path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
            "type":if entry.directory {"directory"} else {"file"},
            "size":entry.size,"modified_at":entry.modified,
            "children":if entry.directory {Some(nested)} else {None},"content":null
        });
        children.entry(parent.to_path_buf()).or_default().push(node);
    }
    let mut files = children.remove(Path::new("output")).unwrap_or_default();
    files.reverse();
    let result = json!({"files":files,"current_path":"/","truncated":truncated});
    if encoded_len(&result)? > MAX_BYTES {
        return Err("FILE_SCAN_LIMIT_EXCEEDED");
    }
    Ok(result)
}

pub fn execute(dir: &Dir, operation: &api::FileOperation) -> Result<Value, &'static str> {
    use api::FileEndpoint as E;
    let r = &operation.request;
    if r.trusted_root.is_some() {
        return Err("TRUSTED_ROOT_REJECTED");
    }
    let path = r.file_path.as_deref().or(r.path.as_deref()).unwrap_or(".");
    relative(path)?;
    let result = match operation.endpoint {
        E::Read => read_op(dir, path, r)?,
        E::Write => {
            let content = r.content.as_deref().ok_or("INVALID_REQUEST")?;
            write(dir, path, content.as_bytes())?;
            json!({"status":"success","message":"File written","file_path":path,"content":content,"formatting_applied":false})
        }
        E::WriteBinary => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(r.content_base64.as_deref().ok_or("INVALID_REQUEST")?)
                .map_err(|_| "INVALID_REQUEST")?;
            write(dir, path, &bytes)?;
            json!({"status":"success","message":"File written","file_path":path})
        }
        E::Edit => edit_op(dir, path, r)?,
        E::Rename => rename_op(dir, r)?,
        E::ListDirectory => {
            let entries = scan(dir, relative(path)?, false)?;
            let text = entries
                .iter()
                .map(|e| format!("{}{}", e.path.display(), if e.directory { "/" } else { "" }))
                .collect::<Vec<_>>()
                .join("\n");
            json!({"status":"success","path":path,"content":text,"ignore_patterns":[]})
        }
        E::Glob => glob(dir, r, path)?,
        E::GlobRead => glob_read(dir, r, path)?,
        E::Grep => grep(dir, r, path)?,
        E::Conventions | E::ConventionChains => {
            json!({"convention_files":conventions(dir,path,r)?})
        }
        E::WriteBatch => return Err("UNSUPPORTED_ENDPOINT"),
    };
    let mut result = result;
    if let Some(fields) = result.as_object_mut()
        && !fields.contains_key("convention_files")
    {
        fields.insert(
            "convention_files".into(),
            Value::Array(conventions(dir, path, r)?),
        );
    }
    Ok(result)
}

fn read_op(dir: &Dir, path: &str, r: &api::FileRequest) -> Result<Value, &'static str> {
    let bytes = read(
        dir,
        path,
        r.max_bytes
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(MAX_BYTES),
    )?;
    if r.binary.unwrap_or(false) {
        return Ok(
            json!({"status":"success", "file_path":path,"content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        );
    }
    let text = String::from_utf8(bytes).map_err(|_| "FILE_BINARY")?;
    let offset = number(r.offset.as_ref(), 0).max(0) as usize;
    let limit = number(r.limit.as_ref(), 2000).max(1) as usize;
    let total_lines = text.lines().count();
    let body = if r.raw.unwrap_or(false) {
        text
    } else {
        text.lines()
            .enumerate()
            .skip(offset)
            .take(limit)
            .map(|(i, line)| format!("{}\t{line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut value = json!({"status":"success","file_path":path,"total_lines":total_lines});
    value["content"] = Value::String(body);
    Ok(value)
}

fn edit_op(dir: &Dir, path: &str, r: &api::FileRequest) -> Result<Value, &'static str> {
    let previous = String::from_utf8(read(dir, path, MAX_BYTES)?).map_err(|_| "FILE_BINARY")?;
    let old = r
        .old_string
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or("INVALID_REQUEST")?;
    let new = r.new_string.as_deref().ok_or("INVALID_REQUEST")?;
    let count = previous.matches(old).count();
    if count == 0 {
        return Err("EDIT_NOT_FOUND");
    }
    if count > 1 && !r.replace_all.unwrap_or(false) {
        return Err("EDIT_AMBIGUOUS");
    }
    let size = previous
        .len()
        .saturating_sub(count.saturating_mul(old.len()))
        .saturating_add(count.saturating_mul(new.len()));
    if size > MAX_BYTES {
        return Err("FILE_TOO_LARGE");
    }
    let updated = previous.replace(old, new);
    write(dir, path, updated.as_bytes())?;
    Ok(
        json!({"status":"success","message":"File edited","file_path":path,"content":updated,"occurrences":count,"formatting_applied":false}),
    )
}

fn rename_op(dir: &Dir, r: &api::FileRequest) -> Result<Value, &'static str> {
    let src = relative(r.src_path.as_deref().ok_or("INVALID_REQUEST")?)?;
    let dst = relative(r.dst_path.as_deref().ok_or("INVALID_REQUEST")?)?;
    if r.overwrite.unwrap_or(false) {
        dir.rename(src, dir, dst).map_err(io_error)?;
    } else {
        dir.hard_link(src, dir, dst).map_err(io_error)?;
        if let Err(error) = dir.remove_file(src) {
            let _ = dir.remove_file(dst);
            return Err(io_error(error));
        }
    }
    Ok(json!({"status":"success","message":"File renamed","src_path":src,"dst_path":dst}))
}

fn conventions(dir: &Dir, path: &str, r: &api::FileRequest) -> Result<Vec<Value>, &'static str> {
    let mut result = Vec::new();
    let path = relative(path)?;
    for file in r.convention_filenames.iter().flatten().take(8) {
        if Path::new(file).components().count() != 1 {
            return Err("TRUSTED_ROOT_REJECTED");
        }
        for parent in path.ancestors().skip(1).take(32) {
            let candidate = parent.join(file);
            match read(dir, &candidate.to_string_lossy(), 20000) {
                Ok(bytes) => {
                    result.push(json!({"path":candidate,"content":String::from_utf8_lossy(&bytes)}))
                }
                Err("FILE_NOT_FOUND") => {}
                Err(error) => return Err(error),
            }
            if result.len() == 12 {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

struct SearchBudget {
    output: usize,
    read: usize,
    truncated: bool,
}

impl SearchBudget {
    fn new(entries: &[Entry]) -> Self {
        Self {
            output: MAX_BYTES,
            read: MAX_SEARCH_BYTES,
            truncated: entries.len() == MAX_ENTRIES,
        }
    }

    fn read(&mut self, dir: &Dir, entry: &Entry) -> Result<Option<Vec<u8>>, &'static str> {
        if entry.size > self.read as u64 {
            self.truncated = true;
            return Ok(None);
        }
        let bytes = read(dir, &entry.path.to_string_lossy(), MAX_BYTES)?;
        self.read = self.read.saturating_sub(bytes.len());
        Ok(Some(bytes))
    }

    fn read_head(&mut self, dir: &Dir, entry: &Entry) -> Result<Option<Vec<u8>>, &'static str> {
        let bytes = read_prefix(dir, &entry.path.to_string_lossy(), MAX_BYTES.min(self.read))?;
        if bytes.is_empty() && entry.size > 0 {
            self.truncated = true;
            return Ok(None);
        }
        self.read = self.read.saturating_sub(bytes.len());
        Ok(Some(bytes))
    }

    fn push<T: serde::Serialize>(
        &mut self,
        rows: &mut Vec<T>,
        row: T,
    ) -> Result<bool, &'static str> {
        let pushed = push_bounded(rows, row, &mut self.output)?;
        self.truncated |= !pushed;
        Ok(pushed)
    }
}

struct GlobMatches<'r> {
    pattern: &'r str,
    files: Vec<Entry>,
    budget: SearchBudget,
}

fn glob_matches<'r>(
    dir: &Dir,
    r: &'r api::FileRequest,
    path: &str,
) -> Result<GlobMatches<'r>, &'static str> {
    let pattern = r.pattern.as_deref().ok_or("INVALID_REQUEST")?;
    let entries = scan(dir, relative(path)?, true)?;
    let budget = SearchBudget::new(&entries);
    relative(pattern)?;
    let matcher = globset::Glob::new(pattern)
        .map_err(|_| "INVALID_PATTERN")?
        .compile_matcher();
    let files = entries
        .into_iter()
        .filter(|e| {
            !e.directory
                && (matcher.is_match(e.path.strip_prefix(path).unwrap_or(&e.path))
                    || matcher.is_match(&e.path))
        })
        .collect();
    Ok(GlobMatches {
        pattern,
        files,
        budget,
    })
}

fn glob_result(matches: &GlobMatches<'_>, path: &str, values: &[Value]) -> Value {
    json!({"status":"success","files":values,"pattern":matches.pattern,"search_path":path,"count":values.len(),"total_count":matches.files.len(),"backstop_hit":matches.budget.truncated})
}

fn glob(dir: &Dir, r: &api::FileRequest, path: &str) -> Result<Value, &'static str> {
    let mut matches = glob_matches(dir, r, path)?;
    let mut values = Vec::new();
    for entry in &matches.files {
        if !matches.budget.push(&mut values, json!(entry.path))? {
            break;
        }
    }
    Ok(glob_result(&matches, path, &values))
}

fn glob_read(dir: &Dir, r: &api::FileRequest, path: &str) -> Result<Value, &'static str> {
    let mut matches = glob_matches(dir, r, path)?;
    let line_limit = r.line_limit.unwrap_or(2000).clamp(1, 2000) as usize;
    let mut values = Vec::new();
    for entry in &matches.files {
        let Some(bytes) = matches.budget.read_head(dir, entry)? else {
            break;
        };
        let content = String::from_utf8_lossy(&bytes)
            .lines()
            .take(line_limit)
            .collect::<Vec<_>>()
            .join("\n");
        let value = json!({"file_path": entry.path, "content": content});
        if !matches.budget.push(&mut values, value)? {
            break;
        }
    }
    Ok(glob_result(&matches, path, &values))
}

#[derive(Clone, Copy)]
enum GrepMode {
    FilesWithMatches,
    Count,
    Content,
}

impl GrepMode {
    fn parse(mode: &str) -> Option<Self> {
        match mode {
            "files_with_matches" => Some(Self::FilesWithMatches),
            "count" => Some(Self::Count),
            "content" => Some(Self::Content),
            _ => None,
        }
    }
}

fn grep(dir: &Dir, r: &api::FileRequest, path: &str) -> Result<Value, &'static str> {
    let pattern = r.pattern.as_deref().ok_or("INVALID_REQUEST")?;
    let mode_name = r.output_mode.as_deref().unwrap_or("files_with_matches");
    let mode = GrepMode::parse(mode_name).ok_or("INVALID_REQUEST")?;
    let entries = scan(dir, relative(path)?, true)?;
    let mut budget = SearchBudget::new(&entries);
    let regex = regex::RegexBuilder::new(pattern)
        .case_insensitive(r.case_insensitive.unwrap_or(false))
        .size_limit(1_000_000)
        .build()
        .map_err(|_| "INVALID_PATTERN")?;
    let glob = r
        .glob
        .as_deref()
        .map(globset::Glob::new)
        .transpose()
        .map_err(|_| "INVALID_PATTERN")?
        .map(|g| g.compile_matcher());
    let limit = number(r.head_limit.as_ref(), 1000).clamp(1, 10000) as usize;
    let line_numbers = r.line_numbers.unwrap_or(false);
    let mut rows = Vec::new();
    for entry in entries.iter().filter(|e| !e.directory) {
        if glob.as_ref().is_some_and(|g| !g.is_match(&entry.path)) {
            continue;
        }
        let bytes = match budget.read(dir, entry) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err("FILE_TOO_LARGE") => continue,
            Err(error) => return Err(error),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let mut matches = text
            .lines()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .peekable();
        if matches.peek().is_none() {
            continue;
        }
        match mode {
            GrepMode::FilesWithMatches => {
                budget.push(&mut rows, entry.path.to_string_lossy().into_owned())?;
            }
            GrepMode::Count => {
                let row = format!("{}:{}", entry.path.display(), matches.count());
                budget.push(&mut rows, row)?;
            }
            GrepMode::Content => {
                for (index, line) in matches {
                    let row = if line_numbers {
                        format!("{}:{}:{line}", entry.path.display(), index + 1)
                    } else {
                        format!("{}:{line}", entry.path.display())
                    };
                    if !budget.push(&mut rows, row)? || rows.len() >= limit {
                        break;
                    }
                }
            }
        }
        if rows.len() >= limit || budget.truncated {
            break;
        }
    }
    Ok(
        json!({"status":"success","results":rows,"pattern":pattern,"search_path":path,"output_mode":mode_name,"count":rows.len(),"total_count":rows.len(),"backstop_hit":budget.truncated || rows.len()==limit}),
    )
}

struct Entry {
    path: PathBuf,
    directory: bool,
    size: u64,
    modified: u64,
}

fn scan(dir: &Dir, root: &Path, recursive: bool) -> Result<Vec<Entry>, &'static str> {
    let mut queue = vec![root.to_path_buf()];
    let mut result = Vec::new();
    let mut path_bytes = 0usize;
    let started = std::time::Instant::now();
    while let Some(parent) = queue.pop() {
        for entry in dir.read_dir(&parent).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let path = parent.join(entry.file_name());
            relative(&path.to_string_lossy())?;
            path_bytes = path_bytes.saturating_add(path.as_os_str().len());
            if path_bytes > MAX_BYTES || started.elapsed() > std::time::Duration::from_secs(5) {
                return Err("FILE_SCAN_LIMIT_EXCEEDED");
            }
            let metadata = dir.symlink_metadata(&path).map_err(io_error)?;
            if metadata.is_symlink() {
                continue;
            }
            let directory = metadata.is_dir();
            if directory && recursive {
                queue.push(path.clone());
            }
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.into_std().duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            result.push(Entry {
                path,
                directory,
                size: metadata.len(),
                modified,
            });
            if result.len() >= MAX_ENTRIES {
                result.sort_by(|a, b| a.path.cmp(&b.path));
                return Ok(result);
            }
        }
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

fn push_bounded<T: serde::Serialize>(
    rows: &mut Vec<T>,
    row: T,
    budget: &mut usize,
) -> Result<bool, &'static str> {
    let size = encoded_len(&row)?.saturating_add(1);
    if size > *budget {
        return Ok(false);
    }
    *budget -= size;
    rows.push(row);
    Ok(true)
}

struct EncodedLen(usize);

impl Write for EncodedLen {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encoded_len<T: serde::Serialize>(value: &T) -> Result<usize, &'static str> {
    let mut length = EncodedLen(0);
    serde_json::to_writer(&mut length, value).map_err(|_| "FILE_OPERATION_FAILED")?;
    Ok(length.0)
}

fn number<T: serde::Serialize>(value: Option<&T>, default: i64) -> i64 {
    value
        .and_then(|x| serde_json::to_value(x).ok())
        .and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(default)
}

fn io_error(error: std::io::Error) -> &'static str {
    match error.kind() {
        std::io::ErrorKind::NotFound => "FILE_NOT_FOUND",
        std::io::ErrorKind::PermissionDenied => "TRUSTED_ROOT_REJECTED",
        std::io::ErrorKind::AlreadyExists => "FILE_ALREADY_EXISTS",
        _ => "FILE_OPERATION_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliverables_preserve_nested_file_nodes_without_reading_large_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        assert_eq!(
            deliverables(&dir).unwrap(),
            json!({"files":[],"current_path":"/"})
        );
        write(&dir, "output/reports/proof.txt", b"proof").unwrap();
        dir.create("output/large.bin")
            .unwrap()
            .set_len((MAX_BYTES + 1) as u64)
            .unwrap();
        let result = deliverables(&dir).unwrap();
        assert_eq!(result["current_path"], "/");
        let report = &result["files"][1];
        assert_eq!(report["type"], "directory");
        assert_eq!(report["path"], "output/reports");
        assert_eq!(report["children"][0]["path"], "output/reports/proof.txt");
        assert_eq!(report["children"][0]["size"], 5);
        assert_eq!(
            artifact_paths(&dir).unwrap_err(),
            "EXECUTOR_ARTIFACT_LIMIT_EXCEEDED"
        );
    }

    #[test]
    fn artifact_scan_cannot_publish_a_partial_directory_tree() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        dir.create_dir("output").unwrap();
        for index in 0..MAX_ENTRIES {
            dir.create_dir(format!("output/d-{index}")).unwrap();
        }
        assert_eq!(
            artifact_paths(&dir).unwrap_err(),
            "EXECUTOR_ARTIFACT_LIMIT_EXCEEDED"
        );
    }

    #[test]
    fn search_stops_before_aggregate_content_exceeds_budget() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        for index in 0..8 {
            write(&dir, &format!("file-{index}.txt"), &vec![b'x'; 400_000]).unwrap();
        }
        let request: api::FileOperation = serde_json::from_value(
            json!({"kind":"file_operation","endpoint":"glob_read","request":{"path":".","pattern":"*.txt"}}),
        )
        .unwrap();
        let result = execute(&dir, &request).unwrap();
        assert_eq!(result["backstop_hit"], true);
        assert!(result["files"].as_array().unwrap().len() < 8);
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_BYTES + 1024);
        let request: api::FileOperation = serde_json::from_value(json!({"kind":"file_operation","endpoint":"grep","request":{"path":".","pattern":"x","output_mode":"content"}})).unwrap();
        let result = execute(&dir, &request).unwrap();
        assert_eq!(result["backstop_hit"], true);
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_BYTES + 1024);
    }

    fn search_fixture() -> (tempfile::TempDir, Dir) {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        write(
            &dir,
            "src/a.rs",
            b"fn alpha() {}\nlet beta = 1;\nFN gamma() {}\n",
        )
        .unwrap();
        write(&dir, "src/nested/b.rs", b"fn delta() {}\n").unwrap();
        write(&dir, "docs/readme.md", b"Alpha docs\nfn in docs\nlast\n").unwrap();
        write(&dir, "bin.dat", &[0xff, 0xfe, b'f', b'n']).unwrap();
        dir.create_dir("empty").unwrap();
        (root, dir)
    }

    const SEARCH_CASES: &[(&str, &str, &str)] = &[
        (
            "glob",
            r#"{"path":".","pattern":"*.rs"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":2,"files":["./src/a.rs","./src/nested/b.rs"],"pattern":"*.rs","search_path":".","status":"success","total_count":2}"#,
        ),
        (
            "glob",
            r#"{"path":"src","pattern":"*.rs"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":2,"files":["src/a.rs","src/nested/b.rs"],"pattern":"*.rs","search_path":"src","status":"success","total_count":2}"#,
        ),
        (
            "glob",
            r#"{"path":".","pattern":"docs/*"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":1,"files":["./docs/readme.md"],"pattern":"docs/*","search_path":".","status":"success","total_count":1}"#,
        ),
        (
            "glob",
            r#"{"path":".","pattern":"*.none"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":0,"files":[],"pattern":"*.none","search_path":".","status":"success","total_count":0}"#,
        ),
        (
            "glob",
            r#"{"path":".","pattern":"../escape"}"#,
            r#"!TRUSTED_ROOT_REJECTED"#,
        ),
        (
            "glob",
            r#"{"path":".","pattern":"["}"#,
            r#"!INVALID_PATTERN"#,
        ),
        ("glob", r#"{"path":"."}"#, r#"!INVALID_REQUEST"#),
        (
            "glob",
            r#"{"path":"missing","pattern":"*"}"#,
            r#"!FILE_NOT_FOUND"#,
        ),
        (
            "glob_read",
            r#"{"path":".","pattern":"src/*.rs","line_limit":1}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":2,"files":[{"content":"fn alpha() {}","file_path":"./src/a.rs"},{"content":"fn delta() {}","file_path":"./src/nested/b.rs"}],"pattern":"src/*.rs","search_path":".","status":"success","total_count":2}"#,
        ),
        (
            "glob_read",
            r#"{"path":".","pattern":"*.dat"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":1,"files":[{"content":"\ufffd\ufffdfn","file_path":"./bin.dat"}],"pattern":"*.dat","search_path":".","status":"success","total_count":1}"#,
        ),
        (
            "glob_read",
            r#"{"path":"docs","pattern":"*.md"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":1,"files":[{"content":"Alpha docs\nfn in docs\nlast","file_path":"docs/readme.md"}],"pattern":"*.md","search_path":"docs","status":"success","total_count":1}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":3,"output_mode":"files_with_matches","pattern":"fn","results":["./docs/readme.md","./src/a.rs","./src/nested/b.rs"],"search_path":".","status":"success","total_count":3}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","glob":"**/*.rs"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":2,"output_mode":"files_with_matches","pattern":"fn","results":["./src/a.rs","./src/nested/b.rs"],"search_path":".","status":"success","total_count":2}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","case_insensitive":true,"output_mode":"count"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":3,"output_mode":"count","pattern":"fn","results":["./docs/readme.md:1","./src/a.rs:2","./src/nested/b.rs:1"],"search_path":".","status":"success","total_count":3}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","output_mode":"content","line_numbers":true}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":3,"output_mode":"content","pattern":"fn","results":["./docs/readme.md:2:fn in docs","./src/a.rs:1:fn alpha() {}","./src/nested/b.rs:1:fn delta() {}"],"search_path":".","status":"success","total_count":3}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","output_mode":"content","head_limit":2}"#,
            r#"{"backstop_hit":true,"convention_files":[],"count":2,"output_mode":"content","pattern":"fn","results":["./docs/readme.md:fn in docs","./src/a.rs:fn alpha() {}"],"search_path":".","status":"success","total_count":2}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","output_mode":"files_with_matches","head_limit":1}"#,
            r#"{"backstop_hit":true,"convention_files":[],"count":1,"output_mode":"files_with_matches","pattern":"fn","results":["./docs/readme.md"],"search_path":".","status":"success","total_count":1}"#,
        ),
        (
            "grep",
            r#"{"path":"src","pattern":"^let","output_mode":"content"}"#,
            r#"{"backstop_hit":false,"convention_files":[],"count":1,"output_mode":"content","pattern":"^let","results":["src/a.rs:let beta = 1;"],"search_path":"src","status":"success","total_count":1}"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","output_mode":"bogus"}"#,
            r#"!INVALID_REQUEST"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"zzz","output_mode":"bogus"}"#,
            r#"!INVALID_REQUEST"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"("}"#,
            r#"!INVALID_PATTERN"#,
        ),
        (
            "grep",
            r#"{"path":".","pattern":"fn","glob":"["}"#,
            r#"!INVALID_PATTERN"#,
        ),
    ];

    #[test]
    fn search_endpoints_match_their_golden_output() {
        let (_root, dir) = search_fixture();
        for (endpoint, request, expected) in SEARCH_CASES {
            let request: Value = serde_json::from_str(request).unwrap();
            let operation: api::FileOperation = serde_json::from_value(
                json!({"kind":"file_operation","endpoint":endpoint,"request":request}),
            )
            .unwrap();
            let expected = match expected.strip_prefix('!') {
                Some(code) => Err(code),
                None => Ok(serde_json::from_str::<Value>(expected).unwrap()),
            };
            assert_eq!(execute(&dir, &operation), expected, "{endpoint} {request}");
        }
    }

    #[test]
    fn content_and_read_return_text_or_base64_for_invalid_utf8() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        write(&dir, "notes/text.txt", "é\nsecond\nthird".as_bytes()).unwrap();
        write(&dir, "blob.bin", &[0x66, 0xff, 0xfe, 0x00]).unwrap();
        let content_of = |path: &str| {
            let request: api::FileContent =
                serde_json::from_value(json!({"kind":"file_content","path":path})).unwrap();
            content(&dir, &request).unwrap()
        };
        assert_eq!(
            content_of("notes/text.txt"),
            json!({"path":"notes/text.txt","name":"text.txt","content":"é\nsecond\nthird","is_binary":false})
        );
        assert_eq!(
            content_of("blob.bin"),
            json!({"path":"blob.bin","name":"blob.bin","content":"Zv/+AA==","is_binary":true})
        );
        let read_with = |request: Value| {
            let operation: api::FileOperation = serde_json::from_value(
                json!({"kind":"file_operation","endpoint":"read","request":request}),
            )
            .unwrap();
            let mut result = execute(&dir, &operation);
            if let Ok(value) = &mut result {
                value.as_object_mut().unwrap().remove("convention_files");
            }
            result
        };
        assert_eq!(
            read_with(json!({"file_path":"notes/text.txt","offset":1,"limit":1})).unwrap(),
            json!({"status":"success","file_path":"notes/text.txt","content":"2\tsecond","total_lines":3})
        );
        assert_eq!(
            read_with(json!({"file_path":"notes/text.txt","raw":true})).unwrap(),
            json!({"status":"success","file_path":"notes/text.txt","content":"é\nsecond\nthird","total_lines":3})
        );
        assert_eq!(
            read_with(json!({"file_path":"blob.bin"})).unwrap_err(),
            "FILE_BINARY"
        );
    }

    #[test]
    fn glob_read_returns_skill_heads_for_the_backend_skill_scan() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        let body = "---\ndescription: Deploy\n---\n".to_string() + &"line\n".repeat(MAX_BYTES / 4);
        std::fs::create_dir_all(root.path().join(".agents/skills/deploy")).unwrap();
        std::fs::write(root.path().join(".agents/skills/deploy/SKILL.md"), &body).unwrap();
        write(
            &dir,
            ".agents/skills/team/review/SKILL.md",
            b"---\ndescription: Review\n---\n",
        )
        .unwrap();
        write(&dir, ".agents/skills/deploy/notes.md", b"skip").unwrap();
        let request: api::FileOperation = serde_json::from_value(json!({"kind":"file_operation","endpoint":"glob_read","request":{"path":".agents/skills","pattern":"**/SKILL.md","line_limit":80}})).unwrap();
        let result = execute(&dir, &request).unwrap();
        let files = result["files"].as_array().unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| f["file_path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                ".agents/skills/deploy/SKILL.md",
                ".agents/skills/team/review/SKILL.md"
            ]
        );
        assert_eq!(files[0]["content"].as_str().unwrap().lines().count(), 80);
        let missing: api::FileOperation = serde_json::from_value(json!({"kind":"file_operation","endpoint":"glob_read","request":{"path":".missing/skills","pattern":"**/SKILL.md"}})).unwrap();
        assert_eq!(execute(&dir, &missing).unwrap_err(), "FILE_NOT_FOUND");
    }

    #[test]
    fn ca_wo_09_confines_files_and_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        write(&dir, "nested/file.txt", b"hello").unwrap();
        assert_eq!(read(&dir, "nested/file.txt", 100).unwrap(), b"hello");
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        for path in ["../bad", "/absolute", "escape/new", "nul\0bad"] {
            assert!(write(&dir, path, b"blocked").is_err());
        }
        assert!(!outside.path().join("new").exists());
    }
}
