pub fn build_http_url(server_ip: &str, server_port: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    if let Some(base) = normalize_base_url(server_ip) {
        return format!("{base}/{path}");
    }
    format!("http://{server_ip}:{server_port}/{path}")
}

pub fn build_ws_url(server_ip: &str, server_port: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    if let Some(base) = normalize_base_url(server_ip) {
        let rest = base
            .strip_prefix("https://")
            .or_else(|| base.strip_prefix("http://"))
            .unwrap_or(&base);
        let scheme = if base.starts_with("https://") {
            "wss://"
        } else {
            "ws://"
        };
        return format!("{scheme}{rest}/{path}");
    }
    format!("ws://{server_ip}:{server_port}/{path}")
}

fn normalize_base_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.trim_end_matches('/').to_string());
    }
    None
}
