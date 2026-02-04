pub fn build_http_url(server_ip: &str, server_port: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    format!("http://{server_ip}:{server_port}/{path}")
}

pub fn build_ws_url(server_ip: &str, server_port: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    format!("ws://{server_ip}:{server_port}/{path}")
}
