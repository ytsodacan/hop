//! Everything that touches files. Every command is `async` so it runs off the UI thread.
use super::index::Index;
use std::fs;
use std::path::{Path, PathBuf};

pub struct FileEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub hidden: bool,
}

fn entry(name: String, md: &fs::Metadata) -> FileEntry {
    #[cfg(windows)]
    let (is_dir, hidden) = {
        use std::os::windows::fs::MetadataExt;
        let a = md.file_attributes();
        (a & 0x10 != 0, a & 0x6 != 0)
    };
    #[cfg(not(windows))]
    let (is_dir, hidden) = (md.is_dir(), name.starts_with('.'));
    FileEntry {
        name,
        dir: is_dir,
        size: if is_dir { 0 } else { md.len() },
        mtime: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        hidden,
    }
}

fn err(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "Access is denied".into(),
        std::io::ErrorKind::NotFound => "Not found".into(),
        std::io::ErrorKind::AlreadyExists => "Already exists".into(),
        _ => e.to_string(),
    }
}

fn bad_name(n: &str) -> bool {
    #[cfg(windows)]
    let bad = "\\/:*?\"<>|";
    #[cfg(not(windows))]
    let bad = "/";
    n.is_empty() || (cfg!(windows) && (n.ends_with('.') || n.ends_with(' '))) || n.chars().any(|c| bad.contains(c) || c.is_control())
}

pub fn list_dir(path: String, idx: &Index) -> Result<Vec<FileEntry>, String> {
    let rd = fs::read_dir(&path).map_err(err)?;
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        let Ok(md) = e.metadata() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let fe = entry(name.clone(), &md);
        if fe.dir {
            dirs.push((e.path().to_string_lossy().into_owned(), name));
        }
        out.push(fe);
    }
    idx.add_many(dirs); // browsing teaches the index
    Ok(out)
}

pub fn stat(path: String) -> Option<FileEntry> {
    let md = fs::metadata(&path).ok()?;
    let p = Path::new(&path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.trim_end_matches(|c| c == '\\' || c == '/').to_string());
    Some(entry(name, &md))
}

pub struct Preview {
    pub text: Option<String>,
    pub size: u64,
}

/// first few KB if it looks like text, otherwise nothing
pub fn preview(path: String) -> Preview {
    use std::io::Read;
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut buf = vec![0u8; 4096];
    let n = fs::File::open(&path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf.truncate(n);
    if n == 0 || buf.contains(&0) {
        return Preview { text: None, size };
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    // a lot of replacement characters means it wasn't text after all
    if text.matches('\u{FFFD}').count() > 4 {
        return Preview { text: None, size };
    }
    Preview { text: Some(text), size }
}

pub fn rename_item(path: String, new_name: String, idx: &Index) -> Result<String, String> {
    if bad_name(&new_name) {
        return Err("A name cannot contain \\ / : * ? \" < > |".into());
    }
    let from = PathBuf::from(&path);
    let to = from.with_file_name(&new_name);
    if to.exists() && !path.eq_ignore_ascii_case(&to.to_string_lossy()) {
        return Err(format!("\"{}\" already exists here", new_name));
    }
    fs::rename(&from, &to).map_err(err)?;
    if to.is_dir() {
        idx.remove_prefix(&path);
        idx.add(&to.to_string_lossy());
    }
    Ok(to.to_string_lossy().into_owned())
}

pub fn delete_items(paths: Vec<String>, idx: &Index) -> Result<(), String> {
    trash::delete_all(&paths).map_err(|e| e.to_string())?;
    for p in &paths {
        idx.remove_prefix(p);
    }
    Ok(())
}

/// undo for delete: pull the newest matching entries back out of the Recycle Bin
pub fn restore_items(paths: Vec<String>) -> Result<(), String> {
    #[cfg(windows)]
    {
    use trash::os_limited::{list, restore_all};
    let all = list().map_err(|e| e.to_string())?;
    let mut pick = Vec::new();
    for p in &paths {
        let best = all
            .iter()
            .filter(|i| i.original_path().to_string_lossy().eq_ignore_ascii_case(p))
            .max_by_key(|i| i.time_deleted);
        if let Some(b) = best {
            pick.push(b.clone());
        }
    }
    if pick.is_empty() {
        return Err("Not in the Recycle Bin any more".into());
    }
    restore_all(pick).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Err("Restore from Trash is not supported on macOS yet".into())
    }
}

pub fn create_item(parent: String, name: String, dir: bool, idx: &Index) -> Result<String, String> {
    if bad_name(&name) {
        return Err("A name cannot contain \\ / : * ? \" < > |".into());
    }
    let p = Path::new(&parent).join(&name);
    if dir {
        fs::create_dir(&p).map_err(err)?;
        idx.add(&p.to_string_lossy());
    } else {
        fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(err)?;
    }
    Ok(p.to_string_lossy().into_owned())
}

/// "name - Copy", "name - Copy (2)" ... or "name (2)" for moves, keeping the extension
fn unique(dir: &Path, name: &str, suffix: Option<&str>) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut i = 1;
    loop {
        let cand = match suffix {
            Some(s) => format!("{stem}{s}{}{ext}", if i > 1 { format!(" ({i})") } else { String::new() }),
            None => format!("{stem} ({}){ext}", i + 1),
        };
        let p = dir.join(cand);
        if !p.exists() {
            return p;
        }
        i += 1;
    }
}

fn copy_rec(from: &Path, to: &Path) -> std::io::Result<()> {
    let md = fs::symlink_metadata(from)?;
    if md.is_dir() {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if md.file_attributes() & 0x400 != 0 { return Ok(()); }
        }
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_rec(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

pub fn copy_items(srcs: Vec<String>, dst: String, idx: &Index) -> Result<Vec<String>, String> {
    let mut made = Vec::new();
    for s in srcs {
        let sp = Path::new(&s);
        let name = sp.file_name().ok_or("bad path")?.to_string_lossy().into_owned();
        let to = unique(Path::new(&dst), &name, Some(" - Copy"));
        // a folder can't be copied into itself
        if to.starts_with(sp) {
            return Err("Cannot copy a folder into itself".into());
        }
        copy_rec(sp, &to).map_err(err)?;
        if to.is_dir() {
            idx.add(&to.to_string_lossy());
        }
        made.push(to.to_string_lossy().into_owned());
    }
    Ok(made)
}

pub struct Moved {
    pub from: String,
    pub to: String,
}

pub fn move_items(srcs: Vec<String>, dst: String, idx: &Index) -> Result<Vec<Moved>, String> {
    let mut done = Vec::new();
    for s in srcs {
        let sp = Path::new(&s);
        let name = sp.file_name().ok_or("bad path")?.to_string_lossy().into_owned();
        let to = unique(Path::new(&dst), &name, None);
        if to.starts_with(sp) {
            return Err("Cannot move a folder into itself".into());
        }
        if fs::rename(sp, &to).is_err() {
            // different drive: copy, then remove the original
            copy_rec(sp, &to).map_err(err)?;
            if sp.is_dir() { fs::remove_dir_all(sp) } else { fs::remove_file(sp) }.map_err(err)?;
        }
        if to.is_dir() {
            idx.remove_prefix(&s);
            idx.add(&to.to_string_lossy());
        }
        done.push(Moved { from: s, to: to.to_string_lossy().into_owned() });
    }
    Ok(done)
}

pub fn open_path(path: String) -> Result<(), String> {
    opener::open(&path).map_err(|e| e.to_string())
}

pub fn reveal(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        opener::open(std::path::Path::new(&path).parent().unwrap_or(std::path::Path::new(&path))).map_err(|e| e.to_string())
    }
}

pub struct Peek {
    pub dirs: u32,
    pub files: u32,
    /// the folder's name when it contains exactly one thing and that thing is a folder
    pub only: Option<String>,
}

/// counts only, nothing is sent back per entry. Cheap even for folders with thousands of items.
pub fn peek(path: String) -> Peek {
    let (mut dirs, mut files, mut last) = (0u32, 0u32, None);
    if let Ok(rd) = fs::read_dir(&path) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    dirs += 1;
                    last = Some(e.file_name().to_string_lossy().into_owned());
                }
                _ => files += 1,
            }
        }
    }
    Peek { dirs, files, only: if dirs == 1 && files == 0 { last } else { None } }
}
