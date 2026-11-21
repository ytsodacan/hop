//! Previews and file facts: shell thumbnails and icons, file details, checksums, folder sizes.
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

fn ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/* ---------- thumbnails ---------- */

/// What Explorer would show for this file: a real thumbnail for pictures, videos and PDFs (through the same cache
/// Explorer uses) and the file's own icon for programs and shortcuts. Raw RGBA, ready for Slint.
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

pub fn thumb(path: &str, size: u32) -> Option<Rgba> {
    #[cfg(windows)]
    {
    // Own thread per request: COM is initialised per thread, and the shared worker threads must stay free of it.
    let px = size.clamp(16, 1024) as i32;
    let path = path.to_string();
    std::thread::spawn(move || shell_image(&path, px)).join().ok()?
    }
    #[cfg(not(windows))]
    {
        let px = size.clamp(16, 1024);
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if ext == "png" {
            if let Ok(mut decoder) = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?)).read_info() {
                let (w, h, color) = {
                    let info = decoder.info();
                    (info.width, info.height, info.color_type)
                };
                let mut buf = vec![0u8; decoder.output_buffer_size().unwrap_or((w*h*4) as usize)];
                let n = decoder.next_frame(&mut buf).ok()?.buffer_size();
                buf.truncate(n);
                let mut data = match color {
                    png::ColorType::Rgb => { let mut rgba = Vec::with_capacity((w*h*4) as usize); for p in buf.chunks_exact(3) { rgba.extend_from_slice(&[p[0],p[1],p[2],255]); } rgba },
                    png::ColorType::Rgba => buf,
                    _ => return None,
                };
                if w != px || h != px {
                    data = nearest_resize(&data, w, h, px, px);
                }
                return Some(Rgba { w: px, h: px, data });
            }
        }
        None
    }
}

#[cfg(not(windows))]
fn nearest_resize(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        let sy = ((y as u64 * sh as u64) / dh as u64).min(sh as u64 - 1) as u32;
        for x in 0..dw {
            let sx = ((x as u64 * sw as u64) / dw as u64).min(sw as u64 - 1) as u32;
            let si = ((sy * sw + sx) * 4) as usize;
            let di = ((y * dw + x) * 4) as usize;
            out[di..di+4].copy_from_slice(&src[si..si+4]);
        }
    }
    out
}

#[cfg(windows)]
struct ComGuard(bool);
#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(windows)]
fn shell_image(path: &str, px: i32) -> Option<Rgba> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::UI::Shell::*;
    unsafe {
        let _guard = ComGuard(CoInitializeEx(None, COINIT_MULTITHREADED).is_ok());
        let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let f: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
        let hbm = f.GetImage(SIZE { cx: px, cy: px }, SIIGBF_BIGGERSIZEOK).ok()?;

        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(hbm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
            let _ = DeleteObject(HGDIOBJ(hbm.0));
            return None;
        }
        let (bw, bh) = (bm.bmWidth, bm.bmHeight.abs());
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = bw;
        bi.bmiHeader.biHeight = -bh; // top-down
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let mut buf = vec![0u8; (bw * bh * 4) as usize];
        let hdc = GetDC(None);
        let got = GetDIBits(hdc, hbm, 0, bh as u32, Some(buf.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, hdc);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        if got == 0 {
            return None;
        }
        // BGRA -> RGBA. A bitmap with no alpha channel at all comes back with alpha 0 everywhere: make it opaque.
        let has_alpha = buf.chunks_exact(4).any(|p| p[3] != 0);
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            }
        }
        Some(Rgba { w: bw as u32, h: bh as u32, data: buf })
    }
}

/* ---------- file facts ---------- */

pub struct ImageDims {
    pub w: usize,
    pub h: usize,
}
pub struct TextStats {
    pub lines: usize,
    pub words: usize,
    pub crlf: bool,
    pub encoding: &'static str,
}
pub struct Info {
    pub size: u64,
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub readonly: bool,
    pub hidden: bool,
    pub image: Option<ImageDims>,
    pub text: Option<TextStats>,
}

pub fn file_info(path: String) -> Result<Info, String> {
    let md = fs::metadata(&path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let (is_dir, readonly, hidden) = {
        use std::os::windows::fs::MetadataExt;
        let a = md.file_attributes();
        (a & 0x10 != 0, md.permissions().readonly(), a & 0x2 != 0)
    };
    #[cfg(not(windows))]
    let (is_dir, readonly, hidden) = (md.is_dir(), md.permissions().readonly(), Path::new(&path).file_name().and_then(|n| n.to_str()).map(|n| n.starts_with('.')).unwrap_or(false));
    let image = if is_dir { None } else { imagesize::size(&path).ok().map(|s| ImageDims { w: s.width, h: s.height }) };
    let text = if is_dir || image.is_some() || md.len() > 2 * 1048576 { None } else { text_stats(&path) };
    Ok(Info {
        size: if is_dir { 0 } else { md.len() },
        created: ms(md.created()),
        modified: ms(md.modified()),
        accessed: ms(md.accessed()),
        readonly,
        hidden,
        image,
        text,
    })
}

fn text_stats(path: &str) -> Option<TextStats> {
    let mut buf = Vec::new();
    fs::File::open(path).ok()?.take(2 * 1048576).read_to_end(&mut buf).ok()?;
    if buf.is_empty() || buf.contains(&0) {
        return None;
    }
    let bom = buf.starts_with(&[0xEF, 0xBB, 0xBF]);
    let s = String::from_utf8_lossy(if bom { &buf[3..] } else { &buf });
    if s.matches('\u{FFFD}').count() > 4 {
        return None;
    }
    let lines = s.matches('\n').count() + usize::from(!s.is_empty() && !s.ends_with('\n'));
    Some(TextStats { lines, words: s.split_whitespace().count(), crlf: s.contains("\r\n"), encoding: if bom { "UTF-8 with BOM" } else { "UTF-8" } })
}

pub fn file_hash(path: String) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/* ---------- folder sizes ---------- */

pub struct Sizes {
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
    /// false when the time budget ran out: the numbers are a lower bound
    pub complete: bool,
}

/// Adds up everything under a folder. With `budget_ms` it stops when time is up and says so, so the UI can show
/// "at least this much" instead of making you wait. Junctions and symlinks are not followed.
pub fn folder_size(path: String, budget_ms: Option<u64>) -> Result<Sizes, String> {
    let md = fs::metadata(&path).map_err(|e| e.to_string())?;
    if md.is_file() {
        return Ok(Sizes { size: md.len(), files: 1, dirs: 0, complete: true });
    }
    let deadline = budget_ms.map(|b| Instant::now() + Duration::from_millis(b));
    let (mut size, mut files, mut dirs) = (0u64, 0u64, 0u64);
    let mut stack = vec![std::path::PathBuf::from(&path)];
    let mut n = 0u32;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            n += 1;
            if n % 512 == 0 {
                if let Some(d) = deadline {
                    if Instant::now() > d {
                        return Ok(Sizes { size, files, dirs, complete: false });
                    }
                }
            }
            let Ok(m) = e.metadata() else { continue };
            #[cfg(windows)]
            let follow = {
                use std::os::windows::fs::MetadataExt;
                let a = m.file_attributes();
                (a & 0x10 != 0, a & 0x400 == 0)
            };
            #[cfg(not(windows))]
            let follow = (m.is_dir(), !m.file_type().is_symlink());
            if follow.0 {
                if follow.1 { dirs += 1; stack.push(e.path()); }
            } else {
                files += 1;
                size += m.len();
            }
        }
    }
    Ok(Sizes { size, files, dirs, complete: true })
}

#[allow(dead_code)]
fn _unused(_: &Path) {}
