//! Small pure formatters shared across views.

/// Wide-track a short label with thin spaces, for the wordmark.
pub fn spaced(s: &str) -> String {
    s.chars()
        .map(|ch| ch.to_string())
        .collect::<Vec<_>>()
        .join("\u{2009}")
}

/// A bandwidth / storage cap, or "unlimited" when unset.
pub fn cap(bytes_per_sec: Option<u64>) -> String {
    match bytes_per_sec {
        Some(v) => format!("{}/s", human_bytes(v)),
        None => "unlimited".into(),
    }
}

/// Byte count in binary units, one decimal above KiB.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
