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
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    let file = dir.open_with(relative(path)?, &options).map_err(io_error)?;
    if !file.metadata().map_err(io_error)?.is_file() {
        return Err("FILE_NOT_REGULAR");
    }
    let mut bytes = Vec::new();
    file.take(limit.min(MAX_BYTES) as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit.min(MAX_BYTES) {
        return Err("FILE_TOO_LARGE");
    }
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
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
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
    let (content, binary) = match String::from_utf8(bytes.clone()) {
        Ok(text) => (text, false),
        Err(_) => (
            base64::engine::general_purpose::STANDARD.encode(bytes),
            true,
        ),
    };
    Ok(
        json!({"path": request.path, "name": Path::new(&request.path).file_name().and_then(|s| s.to_str()).unwrap_or("file"), "content": content, "is_binary": binary}),
    )
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
    if serde_json::to_vec(&result)
        .map_err(|_| "FILE_OPERATION_FAILED")?
        .len()
        > MAX_BYTES
    {
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
        E::Glob | E::GlobRead | E::Grep => search(dir, operation, path)?,
        E::Conventions | E::ConventionChains => {
            json!({"convention_files":conventions(dir,path,r)?})
        }
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
    let offset = number(&r.offset, 0).max(0) as usize;
    let limit = number(&r.limit, 2000).max(1) as usize;
    let lines: Vec<_> = text.lines().collect();
    let body = if r.raw.unwrap_or(false) {
        text.clone()
    } else {
        lines
            .iter()
            .enumerate()
            .skip(offset)
            .take(limit)
            .map(|(i, line)| format!("{}\t{line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(json!({"status":"success","file_path":path,"content":body,"total_lines":lines.len()}))
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
    for file in r.convention_filenames.iter().flatten().take(8) {
        if Path::new(file).components().count() != 1 {
            return Err("TRUSTED_ROOT_REJECTED");
        }
        for parent in relative(path)?.ancestors().skip(1).take(32) {
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

fn search(dir: &Dir, operation: &api::FileOperation, path: &str) -> Result<Value, &'static str> {
    let r = &operation.request;
    let pattern = r.pattern.as_deref().ok_or("INVALID_REQUEST")?;
    let entries = scan(dir, relative(path)?, true)?;
    let files: Vec<_> = entries.iter().filter(|e| !e.directory).collect();
    if operation.endpoint != api::FileEndpoint::Grep {
        relative(pattern)?;
        let matcher = globset::Glob::new(pattern)
            .map_err(|_| "INVALID_PATTERN")?
            .compile_matcher();
        let matches: Vec<_> = files
            .into_iter()
            .filter(|e| {
                matcher.is_match(e.path.strip_prefix(path).unwrap_or(&e.path))
                    || matcher.is_match(&e.path)
            })
            .collect();
        let mut values = Vec::new();
        let mut budget = MAX_BYTES;
        let mut read_budget = MAX_SEARCH_BYTES;
        let mut truncated = entries.len() == MAX_ENTRIES;
        for entry in &matches {
            let value = if operation.endpoint == api::FileEndpoint::GlobRead {
                if entry.size > read_budget as u64 {
                    truncated = true;
                    break;
                }
                let bytes = read(dir, &entry.path.to_string_lossy(), MAX_BYTES)?;
                read_budget = read_budget.saturating_sub(bytes.len());
                json!({"file_path": entry.path, "content": String::from_utf8_lossy(&bytes).lines().take(r.line_limit.unwrap_or(2000).clamp(1, 2000) as usize).collect::<Vec<_>>().join("\n")})
            } else {
                json!(entry.path)
            };
            if !push_bounded(&mut values, value, &mut budget)? {
                truncated = true;
                break;
            }
        }
        return Ok(
            json!({"status":"success","files":values,"pattern":pattern,"search_path":path,"count":values.len(),"total_count":matches.len(),"backstop_hit":truncated}),
        );
    }
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
    let mode = r.output_mode.as_deref().unwrap_or("files_with_matches");
    let limit = number(&r.head_limit, 1000).clamp(1, 10000) as usize;
    let mut rows = Vec::new();
    let mut budget = MAX_BYTES;
    let mut read_budget = MAX_SEARCH_BYTES;
    let mut truncated = entries.len() == MAX_ENTRIES;
    for entry in files {
        if glob.as_ref().is_some_and(|g| !g.is_match(&entry.path)) {
            continue;
        }
        if entry.size > read_budget as u64 {
            truncated = true;
            break;
        }
        let bytes = match read(dir, &entry.path.to_string_lossy(), MAX_BYTES) {
            Ok(bytes) => bytes,
            Err("FILE_TOO_LARGE") => continue,
            Err(error) => return Err(error),
        };
        read_budget = read_budget.saturating_sub(bytes.len());
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let matches: Vec<_> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| regex.is_match(line))
            .collect();
        if matches.is_empty() {
            continue;
        }
        match mode {
            "files_with_matches" => {
                truncated |= !push_bounded(
                    &mut rows,
                    entry.path.to_string_lossy().into_owned(),
                    &mut budget,
                )?;
            }
            "count" => {
                truncated |= !push_bounded(
                    &mut rows,
                    format!("{}:{}", entry.path.display(), matches.len()),
                    &mut budget,
                )?;
            }
            "content" => {
                for (index, line) in matches {
                    let row = if r.line_numbers.unwrap_or(false) {
                        format!("{}:{}:{line}", entry.path.display(), index + 1)
                    } else {
                        format!("{}:{line}", entry.path.display())
                    };
                    if !push_bounded(&mut rows, row, &mut budget)? {
                        truncated = true;
                        break;
                    }
                    if rows.len() >= limit {
                        break;
                    }
                }
            }
            _ => return Err("INVALID_REQUEST"),
        }
        if rows.len() >= limit || truncated {
            break;
        }
    }
    Ok(
        json!({"status":"success","results":rows,"pattern":pattern,"search_path":path,"output_mode":mode,"count":rows.len(),"total_count":rows.len(),"backstop_hit":truncated || rows.len()==limit}),
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
    let size = serde_json::to_vec(&row)
        .map_err(|_| "FILE_OPERATION_FAILED")?
        .len()
        .saturating_add(1);
    if size > *budget {
        return Ok(false);
    }
    *budget -= size;
    rows.push(row);
    Ok(true)
}

fn number<T: serde::Serialize>(value: &Option<T>, default: i64) -> i64 {
    value
        .as_ref()
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
