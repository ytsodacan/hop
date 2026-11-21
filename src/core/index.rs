//! Folder-name index. The UI's go bar asks it "which folders are called something like `ath`?"
//! It is built once in a background thread at low priority, and grows as you browse.
//! Only folder names are kept (path + a normalised name), so it stays a few MB.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const CAP: usize = 250_000;

pub struct Item {
    path: String,
    name: String,
    norm: String,   // lowercase letters+digits only
    words: Vec<u8>, // byte offsets in `norm` where a word starts
    depth: u8,
}

#[derive(Default)]
struct Inner {
    items: Vec<Item>,
    seen: HashSet<String>,
    ready: bool,
}

#[derive(Clone, Default)]
pub struct Index(Arc<Mutex<Inner>>);

/// mirrors normName() in the UI's match.js: split into words on separators, camelCase and digit edges
pub fn norm_name(name: &str) -> (String, Vec<u8>) {
    let mut s = String::new();
    let mut w = Vec::new();
    let mut prev = 0u8;
    for ch in name.chars() {
        if !ch.is_alphanumeric() {
            prev = 0;
            continue;
        }
        let lo: String = ch.to_lowercase().collect();
        let cls = if ch.is_numeric() { 3 } else if lo.chars().next() != Some(ch) { 2 } else { 1 };
        if prev == 0 || (cls == 2 && prev == 1) || (cls == 3 && prev != 3) || (cls != 3 && prev == 3) {
            if s.len() < 256 {
                w.push(s.len() as u8);
            }
        }
        s.push_str(&lo);
        prev = cls;
    }
    (s, w)
}

fn depth_of(path: &str) -> u8 {
    path.split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty()).count().min(255) as u8
}

fn make(path: String, name: String) -> Item {
    let (norm, words) = norm_name(&name);
    let depth = depth_of(&path);
    Item { path, name, norm, words, depth }
}

impl Index {
    pub fn add(&self, path: &str) {
            let name = path.trim_end_matches(|c| c == '\\' || c == '/').rsplit(|c| c == '\\' || c == '/').next().unwrap_or(path).to_string();
        let key = path.to_lowercase();
        let mut g = self.0.lock().unwrap();
        if g.items.len() >= CAP || !g.seen.insert(key) {
            return;
        }
        g.items.push(make(path.to_string(), name));
    }
    pub fn add_many(&self, paths: Vec<(String, String)>) {
        let mut g = self.0.lock().unwrap();
        for (p, n) in paths {
            if g.items.len() >= CAP {
                break;
            }
            if g.seen.insert(p.to_lowercase()) {
                g.items.push(make(p, n));
            }
        }
    }
    /// forget a folder and everything under it
    pub fn remove_prefix(&self, path: &str) {
        let p = path.trim_end_matches(|c| c == '\\' || c == '/').to_lowercase();
        let pre = format!("{p}{}", std::path::MAIN_SEPARATOR);
        let mut g = self.0.lock().unwrap();
        let Inner { items, seen, .. } = &mut *g;
        items.retain(|i| {
            let l = i.path.to_lowercase();
            let drop = l == p || l.starts_with(&pre);
            if drop {
                seen.remove(&l);
            }
            !drop
        });
    }
}

/// match tiers that count as "a real hit" for the UI (>= 640 there): exact, prefix, word start, initials
fn score(tok: &str, tl: usize, it: &Item) -> Option<i32> {
    // cheapest rejections first: this runs over every indexed folder on every lookup
    if it.norm.starts_with(tok) {
        if it.norm.len() == tok.len() {
            return Some(1000);
        }
        let extra = it.norm.chars().count().saturating_sub(tl);
        return Some(860 - (extra as i32 * 3).min(120));
    }
    for &w in &it.words {
        let w = w as usize;
        if w > 0 && w < it.norm.len() && it.norm.is_char_boundary(w) && it.norm[w..].starts_with(tok) {
            return Some(720 - w as i32);
        }
    }
    if tl >= 2 && tl <= it.words.len() {
        let mut wi = 0;
        for c in tok.chars() {
            while wi < it.words.len() && it.norm[it.words[wi] as usize..].chars().next() != Some(c) {
                wi += 1;
            }
            if wi == it.words.len() {
                return None;
            }
            wi += 1;
        }
        return Some(640 - (it.words.len() - tl) as i32 * 10);
    }
    None
}

pub struct Hit {
    pub path: String,
    pub name: String,
    pub depth: u8,
    /// folders between `under` and the hit, when searching below a folder
    pub via: Vec<String>,
}

pub fn lookup(idx: &Index, token: &str, under: Option<String>, max_depth: Option<u8>, limit: Option<usize>) -> Vec<Hit> {
    let tl = token.chars().count();
    if tl == 0 {
        return vec![];
    }
    let limit = limit.unwrap_or(40);
    let under = under.map(|u| u.trim_end_matches(|c| c == '\\' || c == '/').to_string());
    let ud = under.as_deref().map(depth_of).unwrap_or(0);
    let g = idx.0.lock().unwrap();
    let mut hits: Vec<(i32, &Item)> = Vec::new();
    for it in g.items.iter() {
        if let Some(u) = &under {
            let (p, ub) = (it.path.as_bytes(), u.as_bytes());
            if p.len() <= ub.len() + 1 || !matches!(p[ub.len()], b'\\' | b'/') || !p[..ub.len()].eq_ignore_ascii_case(ub) {
                continue;
            }
            if it.depth.saturating_sub(ud) > max_depth.unwrap_or(4) {
                continue;
            }
        }
        if let Some(s) = score(token, tl, it) {
            hits.push((s - it.depth as i32 * 5, it));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    hits.truncate(limit);
    hits.into_iter()
        .map(|(_, it)| {
            let via = match &under {
                Some(u) => {
                    let rel = &it.path[u.len() + 1..];
                    let mut parts: Vec<&str> = rel.split(|c| c == '\\' || c == '/').collect();
                    parts.pop();
                    parts.into_iter().map(String::from).collect()
                }
                None => vec![],
            };
            Hit { path: it.path.clone(), name: it.name.clone(), depth: it.depth, via }
        })
        .collect()
}

impl Index {
    pub fn status(&self) -> (usize, bool) {
        let g = self.0.lock().unwrap();
        (g.items.len(), g.ready)
    }
}

const SKIP: &[&str] = &[
    "node_modules", ".git", ".svn", ".hg", "target", "__pycache__", ".venv", "venv", "site-packages", ".cache",
    "cache", "code cache", "gpucache", "cacheddata", "crashpad", "service worker", "shadercache", "$recycle.bin",
    "system volume information", "winsxs", ".gradle", ".npm", ".rustup", ".nuget", "windowsapps", "$windows.~bt",
];

fn skip_path(lower: &str, name: &str) -> bool {
    SKIP.contains(&name)
        || lower.contains("\\appdata\\local\\temp")
        || lower.contains("/library/caches")
        || lower.contains("/.cache")
        || lower.contains("/node_modules/")
        || lower.contains("/.cargo/registry")
}

fn walk(root: &Path, max_depth: u32, skip_dir: Option<&str>, idx: &Index, skip_windows_root: bool) {
    let mut stack: Vec<(PathBuf, u32)> = vec![(root.to_path_buf(), 0)];
    let mut batch: Vec<(String, String)> = Vec::new();
    while let Some((dir, d)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            #[cfg(windows)]
            let (is_dir, is_junction) = {
            use std::os::windows::fs::MetadataExt;
            let a = md.file_attributes();
            (a & 0x10 != 0, a & 0x400 != 0)
            };
            #[cfg(not(windows))]
            let (is_dir, is_junction) = (md.is_dir(), md.file_type().is_symlink());
            if !is_dir || is_junction {
                continue; // not a folder, or a junction / symlink (avoid loops)
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path().to_string_lossy().into_owned();
            let lower = path.to_lowercase();
            let ln = name.to_lowercase();
            if skip_path(&lower, &ln) || (skip_windows_root && d == 0 && ln == "windows") {
                continue;
            }
            if let Some(s) = skip_dir {
                if lower == s {
                    continue;
                }
            }
            batch.push((path.clone(), name));
            if d + 1 < max_depth {
                stack.push((e.path(), d + 1));
            }
            if batch.len() >= 1500 {
                idx.add_many(std::mem::take(&mut batch));
            }
        }
    }
    idx.add_many(batch);
}

pub fn spawn_scan(idx: Index, done: impl FnOnce(usize) + Send + 'static) {
    let _ = std::thread::Builder::new().name("indexer".into()).spawn(move || {
        // background mode: low CPU *and* low disk priority, so browsing never waits on us
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Threading::*;
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
        let home = dirs::home_dir();
        if let Some(h) = &home {
            idx.add(&h.to_string_lossy());
            walk(h, 9, None, &idx, false);
        }
        let home_l = home.as_ref().map(|h| h.to_string_lossy().to_lowercase());
        #[cfg(windows)]
        for i in 0..26u8 {
            let root = PathBuf::from(format!("{}:\\", (b'A' + i) as char));
            if root.is_dir() { idx.add(&root.to_string_lossy()); walk(&root, 4, home_l.as_deref(), &idx, true); }
        }
        #[cfg(not(windows))]
        {
            let root = if cfg!(target_os = "macos") { PathBuf::from("/") } else { PathBuf::from("/") };
            idx.add(&root.to_string_lossy());
        }
        let count = {
            let mut g = idx.0.lock().unwrap();
            g.ready = true;
            g.items.len()
        };
        done(count);
    });
}
