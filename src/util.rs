//! Path and formatting helpers.
pub const SEP: char = std::path::MAIN_SEPARATOR;

pub fn segs(p: &str) -> Vec<&str> {
    p.split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty()).collect()
}
pub fn leaf(p: &str) -> &str {
    let q = p.trim_end_matches(|c| c == '\\' || c == '/');
    q.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(q)
}
pub fn parent(p: &str) -> Option<String> {
    let q = p.trim_end_matches(|c| c == '\\' || c == '/');
    let i = q.rfind(|c| c == '\\' || c == '/')?;
    if i == 0 { return Some(SEP.to_string()); }
    if i == 2 && q.as_bytes().get(1) == Some(&b':') { return Some(q[..3].to_string()); }
    Some(q[..i].to_string())
}
pub fn join(p: &str, n: &str) -> String {
    if p.ends_with(SEP) { format!("{p}{n}") } else { format!("{p}{SEP}{n}") }
}
pub fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}
pub fn drive_prefix(p: &str) -> String {
    #[cfg(windows)]
    {
        p.get(..2).unwrap_or(p).to_string()
    }
    #[cfg(not(windows))]
    {
        p.to_string()
    }
}
pub fn short_path(p: &str, home: &str) -> String {
    if p.len() > home.len() && p.to_lowercase().starts_with(&home.to_lowercase()) { format!("~{}", &p[home.len()..]) } else { p.to_string() }
}

pub fn fmt_size(b: u64) -> String {
    if b < 1024 {
        return format!("{b} B");
    }
    let (mut v, mut i) = (b as f64, -1i32);
    let units = ["KB", "MB", "GB", "TB"];
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 { format!("{:.1} {}", v, units[i as usize]) } else { format!("{:.0} {}", v, units[i as usize]) }
}

/// "2026-11-19 09:41" in UTC from milliseconds since the epoch
pub fn fmt_date(ms: i64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", if m <= 2 { y + 1 } else { y }, m, d, rem / 3600, (rem % 3600) / 60)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_prefix_is_safe_for_short_paths() {
        assert!(!drive_prefix("/").is_empty());
        assert_eq!(drive_prefix("/"), "/");
    }
}
