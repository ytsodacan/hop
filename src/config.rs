//! Settings, pins, aliases, frecency and per-folder views, saved as JSON in %APPDATA%\hop\config.json.
use crate::core::sys::Known;
use crate::util::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Alias { pub name: String, pub path: String }

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Frec { pub n: u32, pub t: i64 }

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Opts {
    pub lone: bool,
    pub hidden: bool,
    pub pane: bool,
    pub view: String,      // auto | rows | tiles
    pub tile: String,      // s | m | l
    pub folder_sizes: bool,
    pub motion: bool,
    pub theme: String,     // dark | light
    pub density: String,   // tight | normal | loose
    pub aliases_open: bool,
}
impl Default for Opts {
    fn default() -> Self {
        Opts { lone: true, hidden: false, pane: true, view: "auto".into(), tile: "m".into(), folder_sizes: true, motion: true, theme: "dark".into(), density: "normal".into(), aliases_open: true }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub opts: Opts,
    pub pins: Vec<String>,
    pub aliases: Vec<Alias>,
    pub frec: HashMap<String, Frec>,
    pub views: HashMap<String, String>,
}

fn file() -> std::path::PathBuf {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    #[cfg(not(windows))]
    let base = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
    base.join("hop").join("config.json")
}

impl Config {
    pub fn load(k: &Known) -> Config {
        let mut c: Config = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let fresh = c.aliases.is_empty() && c.pins.is_empty();
        if fresh {
            // first run: aliases and pins come from this machine's real folders (only ones that exist)
            #[cfg(windows)]
            {
            let var = |n: &str| k.env.get(n).cloned();
            let roam = var("appdata").unwrap_or_else(|| join(&k.home, "AppData\\Roaming"));
            let local = var("localappdata").unwrap_or_else(|| join(&k.home, "AppData\\Local"));
            let ad = parent(&roam).unwrap_or_else(|| join(&k.home, "AppData"));
            let win = var("windir").unwrap_or_else(|| "C:\\Windows".into());
            let opt = |o: &Option<String>| o.clone();
            let defs: Vec<(&str, Option<String>)> = vec![
                ("appdata", Some(ad.clone())), ("roaming", Some(roam.clone())), ("local", Some(local)), ("locallow", Some(join(&ad, "LocalLow"))), ("temp", var("temp")),
                ("desktop", opt(&k.desktop)), ("docs", opt(&k.documents)), ("dl", opt(&k.downloads)), ("pics", opt(&k.pictures)), ("music", opt(&k.music)), ("vids", opt(&k.videos)),
                ("startup", Some(join(&roam, "Microsoft\\Windows\\Start Menu\\Programs\\Startup"))), ("pf", var("programfiles")), ("pf86", var("programfiles(x86)")),
                ("sys32", Some(join(&win, "System32"))), ("hosts", Some(join(&win, "System32\\drivers\\etc"))), ("home", Some(k.home.clone())), ("dev", Some("C:\\dev".into())),
            ];
            c.aliases = defs.into_iter().filter_map(|(n, p)| p.filter(|p| std::path::Path::new(p).is_dir()).map(|p| Alias { name: n.into(), path: p })).collect();
            c.pins = [&k.documents, &k.downloads, &k.desktop, &k.pictures].iter().filter_map(|p| (*p).clone()).chain(std::iter::once("C:\\dev".to_string())).filter(|p| std::path::Path::new(p).is_dir()).collect();
            }
            #[cfg(not(windows))]
            {
                let defs: Vec<(&str, Option<String>)> = vec![
                    ("desktop", k.desktop.clone()), ("docs", k.documents.clone()), ("dl", k.downloads.clone()),
                    ("pics", k.pictures.clone()), ("music", k.music.clone()), ("vids", k.videos.clone()),
                    ("home", Some(k.home.clone())),
                ];
                c.aliases = defs.into_iter().filter_map(|(n,p)| p.filter(|p| std::path::Path::new(p).is_dir()).map(|p| Alias{name:n.into(), path:p})).collect();
                c.pins = [&k.documents,&k.downloads,&k.desktop,&k.pictures].iter().filter_map(|p| (*p).clone()).filter(|p| std::path::Path::new(p).is_dir()).collect();
            }
        }
        c
    }

    pub fn save(&self) {
        let f = file();
        if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
        if let Ok(s) = serde_json::to_string_pretty(self) { let _ = std::fs::write(f, s); }
    }

    pub fn frec_score(&self, p: &str) -> f64 {
        let Some(f) = self.frec.get(p) else { return 0.0 };
        let age = (now_ms() - f.t) as f64;
        let (h, d) = (3_600_000.0, 86_400_000.0);
        f.n as f64 * if age < h { 4.0 } else if age < d { 2.0 } else if age < 7.0 * d { 0.5 } else { 0.25 }
    }
    pub fn boost(&self, p: &str) -> f64 { (25.0 * (1.0 + self.frec_score(p)).log2()).min(150.0) }
    pub fn bump(&mut self, p: &str) {
        let f = self.frec.entry(p.to_string()).or_default();
        f.n += 1;
        f.t = now_ms();
    }
}
