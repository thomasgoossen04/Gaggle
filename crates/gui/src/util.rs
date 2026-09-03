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

/// Bytes/sec as a human rate, or "—" for zero.
pub fn human_rate(n: u64) -> String {
    if n == 0 {
        "—".into()
    } else {
        format!("{}/s", human_bytes(n))
    }
}

// --- Settings-form parsing (empty / unparseable ⇒ `None`, i.e. unlimited/off) --

/// A MiB/s figure as bytes/sec.
pub fn parse_rate_mib(s: &str) -> Option<u64> {
    let t = s.trim();
    (!t.is_empty())
        .then(|| t.parse::<f64>().ok())
        .flatten()
        .filter(|v| *v > 0.0)
        .map(|v| (v * (1u64 << 20) as f64) as u64)
}

/// A GiB figure as bytes.
pub fn parse_size_gib(s: &str) -> Option<u64> {
    let t = s.trim();
    (!t.is_empty())
        .then(|| t.parse::<f64>().ok())
        .flatten()
        .filter(|v| *v > 0.0)
        .map(|v| (v * (1u64 << 30) as f64) as u64)
}

/// A minutes figure as seconds.
pub fn parse_minutes(s: &str) -> Option<u64> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.parse::<u64>().ok()).flatten().filter(|v| *v > 0).map(|m| m * 60)
}

/// Inverse of [`parse_rate_mib`] for pre-filling the field.
pub fn fmt_rate_mib(bytes_per_sec: Option<u64>) -> String {
    match bytes_per_sec {
        Some(v) => format!("{:.0}", v as f64 / (1u64 << 20) as f64),
        None => String::new(),
    }
}

pub fn fmt_size_gib(bytes: Option<u64>) -> String {
    match bytes {
        Some(v) => format!("{:.0}", v as f64 / (1u64 << 30) as f64),
        None => String::new(),
    }
}

pub fn fmt_minutes(secs: Option<u64>) -> String {
    match secs {
        Some(v) => format!("{}", v / 60),
        None => String::new(),
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
