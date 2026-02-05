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
    #[allow(unused)] server_port: &str,
    path: &str,
) -> Result<String, String> {
    let path = path.trim_start_matches('/');

    let mut server_ip = server_ip.trim().to_string();

    if server_ip.is_empty() {
        return Err("Server address is empty.".to_string());
    }

    // Fix common broken scheme: "http//localhost"
    if server_ip.starts_with("http//") {
        server_ip = server_ip.replacen("http//", "http://", 1);
    } else if server_ip.starts_with("https//") {
        server_ip = server_ip.replacen("https//", "https://", 1);
    }

    // 1. Explicit scheme → trust it, ignore server_port
    if server_ip.starts_with("http://") || server_ip.starts_with("https://") {
        let base =
            Url::parse(&server_ip).map_err(|_| format!("Invalid server URL: {}", server_ip))?;

        return base
            .join(path)
            .map_err(|_| "Failed to build request URL.".to_string())
            .map(|u| u.to_string());
    }

    // 2. Port embedded in server_ip → assume http
    if server_ip.contains(':') {
        let base = Url::parse(&format!("http://{}", server_ip))
            .map_err(|_| format!("Invalid server address: {}", server_ip))?;

        return base
            .join(path)
            .map_err(|_| "Failed to build request URL.".to_string())
            .map(|u| u.to_string());
    }

    // 3. Bare hostname → https, ignore server_port completely
    let base = Url::parse(&format!("https://{}", server_ip))
        .map_err(|_| "Invalid server address.".to_string())?;

    base.join(path)
        .map_err(|_| "Failed to build request URL.".to_string())
        .map(|u| u.to_string())
}

pub fn build_ws_url(server_ip: &str, _server_port: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    let mut server_ip = server_ip.trim().to_string();

    // Fix common broken scheme
    if server_ip.starts_with("http//") {
        server_ip = server_ip.replacen("http//", "http://", 1);
    } else if server_ip.starts_with("https//") {
        server_ip = server_ip.replacen("https//", "https://", 1);
    }

    // 1. Explicit scheme → trust it
    if server_ip.starts_with("https://") {
        let rest = server_ip.trim_start_matches("https://");
        return format!("wss://{}/{}", rest, path);
    }

    if server_ip.starts_with("http://") {
        let rest = server_ip.trim_start_matches("http://");
        return format!("ws://{}/{}", rest, path);
    }

    // 2. Port embedded in server_ip → ws
    if server_ip.contains(':') {
        return format!("ws://{}/{}", server_ip, path);
    }

    // 3. Bare hostname → wss, ignore server_port
    format!("wss://{}/{}", server_ip, path)
}
