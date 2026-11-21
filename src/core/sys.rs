//! Machine facts: known folders, environment, drives.
use std::collections::HashMap;

pub struct Known {
    pub home: String,
    pub desktop: Option<String>,
    pub documents: Option<String>,
    pub downloads: Option<String>,
    pub pictures: Option<String>,
    pub music: Option<String>,
    pub videos: Option<String>,
    /// every environment variable, lower-cased name -> value, for %VAR% expansion
    pub env: HashMap<String, String>,
}

fn s(p: Option<std::path::PathBuf>) -> Option<String> {
    p.map(|p| p.to_string_lossy().into_owned())
}

pub fn known() -> Known {
    #[cfg(windows)]
    let home_fallback = "C:\\".to_string();
    #[cfg(not(windows))]
    let home_fallback = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Known {
        home: s(dirs::home_dir()).unwrap_or(home_fallback),
        desktop: s(dirs::desktop_dir()),
        documents: s(dirs::document_dir()),
        downloads: s(dirs::download_dir()),
        pictures: s(dirs::picture_dir()),
        music: s(dirs::audio_dir()),
        videos: s(dirs::video_dir()),
        env: std::env::vars().map(|(k, v)| (k.to_lowercase(), v)).collect(),
    }
}

pub struct Drive {
    pub path: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
    pub kind: u32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn drives() -> Vec<Drive> {
    #[cfg(windows)]
    {
    use windows_sys::Win32::Storage::FileSystem::*;
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let path = format!("{}:\\", (b'A' + i as u8) as char);
        let w = wide(&path);
        let kind = unsafe { GetDriveTypeW(w.as_ptr()) };
        // 2 removable, 3 fixed, 4 network. skip optical / ram / unknown
        if !(kind == 2 || kind == 3 || kind == 4) {
            continue;
        }
        let (mut free, mut total, mut tfree) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, &mut total, &mut tfree) };
        if ok == 0 {
            continue; // empty card reader etc.
        }
        let mut name = [0u16; 64];
        let got = unsafe {
            GetVolumeInformationW(
                w.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        let label = if got != 0 {
            let n = name.iter().position(|&c| c == 0).unwrap_or(0);
            String::from_utf16_lossy(&name[..n])
        } else {
            String::new()
        };
        out.push(Drive { path, label, total, free: tfree, kind });
    }
    out
    }

    #[cfg(not(windows))]
    {
        let mut out = Vec::new();
        #[cfg(target_os = "macos")]
        let roots = ["/", "/Volumes"];
        #[cfg(not(target_os = "macos"))]
        let roots = ["/"];
        for root in roots {
            if let Ok(md) = std::fs::metadata(root) {
                let mut total = md.len();
                let mut free = 0;
                let kind = if root == "/" { 3 } else { 2 };
                out.push(Drive { path: root.into(), label: if root == "/" { "Macintosh HD".into() } else { "Volumes".into() }, total, free, kind });
                if root == "/" { break; }
            }
        }
        out
    }
}

/// `hop.exe C:\some\folder` opens there
pub fn initial_path() -> Option<String> {
    std::env::args().nth(1).filter(|p| std::path::Path::new(p).is_dir())
}
