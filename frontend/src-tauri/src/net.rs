use reqwest::Url;

pub fn build_http_url(server_ip: &str, server_port: &str, path: &str) -> String {
    match build_http_url_checked(server_ip, server_port, path) {
        Ok(url) => url,
        Err(err) => {
            eprintln!("URL build error: {}", err);
            "http://localhost/".to_string()
        }
    }
}

pub fn build_http_url_checked(
    server_ip: &str,
    server_port: &str,
    path: &str,
) -> Result<String, String> {
    let path = path.trim_start_matches('/');

    let mut server_ip = server_ip.trim().to_string();
    let server_port = server_port.trim();

    if server_ip.is_empty() {
        return Err("Server address is empty.".to_string());
    }

    // --- HARD NORMALIZATION ---

    // Fix common broken scheme: "http//localhost"
    if server_ip.starts_with("http//") {
        server_ip = server_ip.replacen("http//", "http://", 1);
    } else if server_ip.starts_with("https//") {
        server_ip = server_ip.replacen("https//", "https://", 1);
    }

    // If it already looks like a full URL, trust it and IGNORE server_port
    if server_ip.starts_with("http://") || server_ip.starts_with("https://") {
        let base =
            Url::parse(&server_ip).map_err(|_| format!("Invalid server URL: {}", server_ip))?;

        return base
            .join(path)
            .map_err(|_| "Failed to build request URL.".to_string())
            .map(|u| u.to_string());
    }

    // If server_ip already contains a port (localhost:2121), ignore server_port
    if server_ip.contains(':') {
        let base = Url::parse(&format!("http://{}", server_ip))
            .map_err(|_| format!("Invalid server address: {}", server_ip))?;

        return base
            .join(path)
            .map_err(|_| "Failed to build request URL.".to_string())
            .map(|u| u.to_string());
    }

    // Fallback: host + separate port
    let port = if !server_port.is_empty() {
        server_port
    } else {
        // last-resort default
        "80"
    };

    let base = Url::parse(&format!("http://{}:{}", server_ip, port))
        .map_err(|_| "Invalid server address.".to_string())?;

    base.join(path)
        .map_err(|_| "Failed to build request URL.".to_string())
        .map(|u| u.to_string())
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
