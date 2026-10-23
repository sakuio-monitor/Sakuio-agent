use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 500;
const MAX_SEARCH_DIRS: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified_at: String,
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub trash_id: String,
    pub name: String,
    pub original_path: String,
    pub trashed_at: String,
    pub is_dir: bool,
}

pub fn execute_file_op(op: &str, args: &Value) -> Result<Value, String> {
    match op {
        "list_roots" => list_roots(),
        "list" => list_dir(args),
        "stat" => stat_path(arg_path(args, "path")?),
        "mkdir" => mkdir(arg_path(args, "path")?),
        "delete" => delete_path(args),
        "move" => {
            let from = arg_string(args, "from")?;
            let to = arg_string(args, "to")?;
            move_path(&from, &to)
        }
        "rename" => {
            let from = arg_string(args, "from")?;
            let to = arg_string(args, "to")?;
            move_path(&from, &to)
        }
        "read_text" => read_text(arg_path(args, "path")?),
        "write_text" => {
            let path = arg_path(args, "path")?;
            let content = arg_string(args, "content")?;
            write_text(path, &content)
        }
        "chmod" => chmod_path(args),
        "chown" => chown_path(args),
        "copy" => {
            let from = arg_string(args, "from")?;
            let to = arg_string(args, "to")?;
            copy_path(&from, &to)
        }
        "search" => search_files(args),
        "trash_list" => trash_list(),
        "trash_restore" => trash_restore(arg_string(args, "trash_id")?),
        "trash_empty" => trash_empty(),
        "trash_delete" => trash_delete(arg_string(args, "trash_id")?),
        "stream_read" => Err("stream_read is handled via relay".into()),
        "stream_write" => Err("stream_write is handled via relay".into()),
        _ => Err(format!("unsupported op: {op}")),
    }
}

fn arg_path(args: &Value, key: &str) -> Result<String, String> {
    arg_string(args, key)
}

fn arg_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing {key}"))
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn arg_u64(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

fn resolve_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(PathBuf::from("/"));
    }
    let path = PathBuf::from(trimmed);
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err("path traversal denied".into());
        }
    }
    Ok(path)
}

fn data_home() -> PathBuf {
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h);
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        return PathBuf::from(h);
    }
    PathBuf::from("/tmp")
}

fn trash_root() -> PathBuf {
    let primary = data_home().join(".sakuio").join("trash");
    if fs::create_dir_all(&primary).is_ok() {
        return primary;
    }
    // HOME may be unwritable (system service, sandboxed agent, read-only
    // container). Fall back to the platform temp directory so deletion still
    // works instead of failing before touching the source file.
    std::env::temp_dir().join("sakuio-trash")
}

#[cfg(windows)]
fn list_roots() -> Result<Value, String> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    let mask = unsafe { GetLogicalDrives() };
    let mut roots = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let name = format!("{}:\\", (b'A' + index as u8) as char);
        roots.push(json!({ "name": name, "path": name, "is_dir": true }));
    }
    Ok(json!(roots))
}

#[cfg(not(windows))]
fn list_roots() -> Result<Value, String> {
    Ok(json!([{ "name": "/", "path": "/", "is_dir": true }]))
}

fn list_dir(args: &Value) -> Result<Value, String> {
    let raw = arg_path(args, "path")?;
    let offset = arg_u64(args, "offset", 0) as usize;
    let limit = arg_u64(args, "limit", 200).min(2000) as usize;
    let path = resolve_path(&raw)?;
    let read = fs::read_dir(&path).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        if let Ok(info) = describe(entry.path()) {
            entries.push(info);
        }
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    let total = entries.len();
    let end = (offset + limit).min(total);
    let slice = if offset >= total {
        &[][..]
    } else {
        &entries[offset..end]
    };
    Ok(json!({ "total": total, "items": slice }))
}

fn stat_path(raw: String) -> Result<Value, String> {
    let path = resolve_path(&raw)?;
    let info = describe(path)?;
    Ok(serde_json::to_value(info).map_err(|e| e.to_string())?)
}

fn describe(path: PathBuf) -> Result<FileEntry, String> {
    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let modified_at = meta
        .modified()
        .ok()
        .map(|t| format!("{:?}", t))
        .unwrap_or_default();
    Ok(FileEntry {
        name,
        path: path.to_string_lossy().to_string(),
        is_dir: meta.is_dir(),
        is_symlink: meta.file_type().is_symlink(),
        size: meta.len(),
        modified_at,
        mode: file_mode_string(&path),
    })
}

fn file_mode_string(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).ok()?.permissions().mode();
        Some(format!("{:o}", mode & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

fn mkdir(raw: String) -> Result<Value, String> {
    let path = resolve_path(&raw)?;
    fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    Ok(json!({ "created": true }))
}

fn delete_path(args: &Value) -> Result<Value, String> {
    let raw = arg_path(args, "path")?;
    let permanent = arg_bool(args, "permanent");
    let path = resolve_path(&raw)?;
    if path == Path::new("/") {
        return Err("refusing to delete root".into());
    }
    if permanent {
        remove_path_all(&path)?;
        return Ok(json!({ "deleted": true, "permanent": true }));
    }
    trash_move(path)
}

fn remove_path_all(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    } else {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn trash_move(path: PathBuf) -> Result<Value, String> {
    let trash = trash_root();
    fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("item")
        .to_string();
    let dest = trash.join(format!("{id}_{name}"));
    move_entry(&path, &dest)?;
    fs::write(
        trash.join(format!("{id}.origin")),
        path.to_string_lossy().as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({ "trashed": true, "trash_id": id }))
}

fn trash_list() -> Result<Value, String> {
    let trash = trash_root();
    if !trash.is_dir() {
        return Ok(json!([]));
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(&trash).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".origin") {
            continue;
        }
        let trash_id = name.trim_end_matches(".origin").to_string();
        let origin_path = trash.join(&name);
        let original_path =
            String::from_utf8(fs::read(&origin_path).map_err(|e| e.to_string())?)
                .map_err(|_| "invalid origin".to_string())?;
        let payload_path = trash_dir_entry_path(&trash, &trash_id)?;
        let meta = fs::metadata(&payload_path).map_err(|e| e.to_string())?;
        let display_name = Path::new(&original_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        items.push(TrashEntry {
            trash_id: trash_id.clone(),
            name: display_name,
            original_path,
            trashed_at: meta
                .modified()
                .ok()
                .map(|t| format!("{:?}", t))
                .unwrap_or_default(),
            is_dir: meta.is_dir(),
        });
    }
    items.sort_by(|a, b| b.trash_id.cmp(&a.trash_id));
    Ok(serde_json::to_value(items).map_err(|e| e.to_string())?)
}

fn trash_dir_entry_path(trash: &Path, trash_id: &str) -> Result<PathBuf, String> {
    for entry in fs::read_dir(trash).map_err(|e| e.to_string())?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&format!("{trash_id}_")) && !name.ends_with(".origin") {
            return Ok(entry.path());
        }
    }
    Err("trash item not found".into())
}

fn trash_restore(trash_id: String) -> Result<Value, String> {
    let trash = trash_root();
    let payload = trash_dir_entry_path(&trash, &trash_id)?;
    let origin_file = trash.join(format!("{trash_id}.origin"));
    let original = String::from_utf8(fs::read(&origin_file).map_err(|e| e.to_string())?)
        .map_err(|_| "invalid origin".to_string())?;
    let dst = resolve_path(&original)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if dst.exists() {
        return Err("restore target already exists".into());
    }
    move_entry(&payload, &dst)?;
    let _ = fs::remove_file(&origin_file);
    Ok(json!({ "restored": true, "path": dst.to_string_lossy() }))
}

fn trash_delete(trash_id: String) -> Result<Value, String> {
    let trash = trash_root();
    let payload = trash_dir_entry_path(&trash, &trash_id)?;
    remove_path_all(&payload)?;
    let _ = fs::remove_file(trash.join(format!("{trash_id}.origin")));
    Ok(json!({ "deleted": true }))
}

fn trash_empty() -> Result<Value, String> {
    let trash = trash_root();
    if trash.is_dir() {
        fs::remove_dir_all(&trash).map_err(|e| e.to_string())?;
    }
    Ok(json!({ "emptied": true }))
}

fn copy_path(from: &str, to: &str) -> Result<Value, String> {
    let src = resolve_path(from)?;
    let dst = resolve_path(to)?;
    if dst.starts_with(&src) {
        return Err("cannot copy a directory into itself".into());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    copy_entry(&src, &dst)?;
    Ok(json!({ "copied": true }))
}

fn copy_entry(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if meta.file_type().is_symlink() {
        use std::os::unix::fs::symlink;
        let target = fs::read_link(src).map_err(|e| e.to_string())?;
        if dst.exists() {
            remove_path_all(dst)?;
        }
        return symlink(target, dst).map_err(|e| e.to_string());
    }
    #[cfg(not(unix))]
    if meta.file_type().is_symlink() {
        // Windows symlinks/junctions may require privileges to create. Copy
        // the target instead so file operations still work for normal users.
        let target = fs::metadata(src).map_err(|e| e.to_string())?;
        if target.is_dir() {
            fs::create_dir_all(dst).map_err(|e| e.to_string())?;
            for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
                copy_entry(&entry.path(), &dst.join(entry.file_name()))?;
            }
            copy_permissions(src, dst);
            return Ok(());
        }
        fs::copy(src, dst).map_err(|e| e.to_string())?;
        copy_permissions(src, dst);
        return Ok(());
    }
    if meta.is_dir() {
        fs::create_dir_all(dst).map_err(|e| e.to_string())?;
        for entry in fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
            let name = entry.file_name();
            copy_entry(&entry.path(), &dst.join(name))?;
        }
        copy_permissions(src, dst);
        return Ok(());
    }
    fs::copy(src, dst).map_err(|e| e.to_string())?;
    copy_permissions(src, dst);
    Ok(())
}

fn copy_permissions(src: &Path, dst: &Path) {
    if let Ok(meta) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, meta.permissions());
    }
}

fn move_path(from: &str, to: &str) -> Result<Value, String> {
    let src = resolve_path(from)?;
    let dst = resolve_path(to)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    move_entry(&src, &dst)?;
    Ok(json!({ "moved": true }))
}

/// Rename first; if source and destination are on different filesystems,
/// copy recursively and only then remove the source.
fn move_entry(src: &Path, dst: &Path) -> Result<(), String> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(rename_error) => {
            copy_entry(src, dst).map_err(|copy_error| {
                format!("rename failed: {rename_error}; copy fallback failed: {copy_error}")
            })?;
            remove_path_all(src)
        }
    }
}

fn chmod_path(args: &Value) -> Result<Value, String> {
    let path = resolve_path(&arg_path(args, "path")?)?;
    let mode_str = arg_string(args, "mode")?;
    let mode_digits = mode_str.strip_prefix("0o").unwrap_or(&mode_str);
    let mode = u32::from_str_radix(mode_digits, 8).map_err(|_| "invalid mode".to_string())?;
    apply_mode(&path, mode)?;
    Ok(json!({ "mode": format!("{:o}", mode) }))
}

#[cfg(unix)]
fn chown_path(args: &Value) -> Result<Value, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = resolve_path(&arg_path(args, "path")?)?;
    let owner = args.get("owner").and_then(Value::as_str).unwrap_or("").trim();
    let group = args.get("group").and_then(Value::as_str).unwrap_or("").trim();
    let uid = resolve_user(owner)?;
    let gid = resolve_group(group)?;
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let uid = uid.map(|value| value as libc::uid_t).unwrap_or(libc::uid_t::MAX);
    let gid = gid.map(|value| value as libc::gid_t).unwrap_or(libc::gid_t::MAX);
    let result = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(json!({ "chown": true }))
}

#[cfg(unix)]
fn resolve_user(value: &str) -> Result<Option<u32>, String> {
    use std::ffi::CString;
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return value.parse::<u32>().map(Some).map_err(|e| e.to_string());
    }
    let name = CString::new(value).map_err(|e| e.to_string())?;
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        return Err(format!("unknown user: {value}"));
    }
    Ok(Some(unsafe { (*entry).pw_uid }))
}

fn resolve_group(value: &str) -> Result<Option<u32>, String> {
    use std::ffi::CString;
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return value.parse::<u32>().map(Some).map_err(|e| e.to_string());
    }
    let name = CString::new(value).map_err(|e| e.to_string())?;
    let entry = unsafe { libc::getgrnam(name.as_ptr()) };
    if entry.is_null() {
        return Err(format!("unknown group: {value}"));
    }
    Ok(Some(unsafe { (*entry).gr_gid }))
}

#[cfg(not(unix))]
fn chown_path(_args: &Value) -> Result<Value, String> {
    Err("chown is not supported on this platform".into())
}

fn apply_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms).map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let readonly = mode & 0o200 == 0;
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        let mut permissions = meta.permissions();
        permissions.set_readonly(readonly);
        fs::set_permissions(path, permissions).map_err(|e| e.to_string())
    }
}

fn search_files(args: &Value) -> Result<Value, String> {
    let root = resolve_path(&arg_path(args, "path")?)?;
    let query = arg_string(args, "query")?.to_lowercase();
    if query.is_empty() {
        return Ok(json!([]));
    }
    let mut results = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root);
    let mut visited_dirs = 0usize;
    while let Some(dir) = queue.pop_front() {
        visited_dirs += 1;
        if visited_dirs > MAX_SEARCH_DIRS || results.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        let read = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.contains(&query) {
                if let Ok(info) = describe(path.clone()) {
                    results.push(info);
                    if results.len() >= MAX_SEARCH_RESULTS {
                        break;
                    }
                }
            }
            if path.is_dir() {
                queue.push_back(path);
            }
        }
    }
    Ok(serde_json::to_value(results).map_err(|e| e.to_string())?)
}

fn read_text(raw: String) -> Result<Value, String> {
    let path = resolve_path(&raw)?;
    let meta = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() as usize > MAX_TEXT_BYTES {
        return Err(format!("file exceeds {MAX_TEXT_BYTES} byte text limit"));
    }
    let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let content = String::from_utf8(buf).map_err(|_| "not valid utf-8".to_string())?;
    Ok(json!({ "content": content, "encoding": "utf-8" }))
}

fn write_text(raw: String, content: &str) -> Result<Value, String> {
    let path = resolve_path(&raw)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(json!({ "written": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sakuio-fs-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).expect("temp root");
        root
    }

    #[test]
    fn copy_keeps_source_and_copies_contents() {
        let root = temp_root("copy");
        let src = root.join("source");
        let dst = root.join("destination");
        fs::create_dir_all(&src).expect("source dir");
        fs::write(src.join("file.txt"), b"payload").expect("source file");
        copy_path(src.to_str().unwrap(), dst.to_str().unwrap()).expect("copy");
        assert_eq!(fs::read(dst.join("file.txt")).expect("copied file"), b"payload");
        assert!(src.join("file.txt").is_file(), "copy must not remove source");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn chmod_accepts_leading_zeroes() {
        #[cfg(unix)]
        {
            let root = temp_root("chmod");
            let file = root.join("file.txt");
            fs::write(&file, b"x").expect("file");
            chmod_path(&serde_json::json!({ "path": file, "mode": "0000" })).expect("chmod 0000");
            chmod_path(&serde_json::json!({ "path": file, "mode": "0644" })).expect("chmod 0644");
            let _ = fs::remove_dir_all(&root);
        }
    }
}
