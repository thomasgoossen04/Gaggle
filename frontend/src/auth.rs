use wasm_bindgen::prelude::JsValue;
use web_sys::{window, UrlSearchParams};
use yew::UseStateHandle;

use crate::app::{AppState, User};
use wasm_bindgen_futures::spawn_local;

use crate::api::{get_json, send_json, send_request};
use crate::net::build_http_url;

pub const SERVER_IP_KEY: &str = "gaggle_server_ip";
pub const SERVER_PORT_KEY: &str = "gaggle_server_port";
pub const SESSION_TOKEN_KEY: &str = "gaggle_session_token";
pub const LOGIN_SUCCESS_KEY: &str = "gaggle_login_success";

pub fn handle_login(server_ip: &str, server_port: &str) {
    let server_ip = server_ip.trim();
    let server_port = server_port.trim();
    if server_ip.is_empty() || server_port.is_empty() {
        return;
    }

    set_local_storage_item(SERVER_IP_KEY, server_ip);
    set_local_storage_item(SERVER_PORT_KEY, server_port);

    let redirect = current_app_url();
    let redirect = js_sys::encode_uri_component(&redirect)
        .as_string()
        .unwrap_or_default();
    let login_url = build_http_url(
        server_ip,
        server_port,
        &format!("auth/discord/login?redirect={redirect}"),
    );

    if let Some(win) = window() {
        if win.location().set_href(&login_url).is_ok() {
            return;
        }
        let _ = win.open_with_url_and_target(&login_url, "_blank");
    }
}

pub fn handle_logout(app_state: UseStateHandle<AppState>) {
    let server_ip = app_state.server_ip.clone();
    let server_port = app_state.server_port.clone();
    let token = app_state.session_token.clone();
    if let (Some(server_ip), Some(server_port), Some(token)) = (server_ip, server_port, token) {
        spawn_local(async move {
            let url = build_http_url(&server_ip, &server_port, "auth/logout");
            let _ = send_json("POST", &url, Some(&token), None).await;
        });
    }
    remove_local_storage_item(SESSION_TOKEN_KEY);
    app_state.set(AppState {
        logged_in: false,
        server_ip: get_local_storage_item(SERVER_IP_KEY),
        server_port: get_local_storage_item(SERVER_PORT_KEY)
            .or_else(|| Some("2121".to_string())),
        session_token: None,
        auth_error: None,
        user: None,
    });
}

pub fn current_app_url() -> String {
    if let Some(win) = window() {
        if let Ok(location) = win.location().href() {
            return location.split('?').next().unwrap_or("").to_string();
        }
    }
    String::new()
}

pub fn get_query_param(name: &str) -> Option<String> {
    let win = window()?;
    let search = win.location().search().ok()?;
    let params = UrlSearchParams::new_with_str(&search).ok()?;
    params.get(name)
}

pub fn clear_query_param(name: &str) {
    let win = match window() {
        Some(win) => win,
        None => return,
    };
    let location = match win.location().href() {
        Ok(href) => href,
        Err(_) => return,
    };
    let mut parts = location.splitn(2, '?');
    let base = parts.next().unwrap_or("");
    let search = parts.next().unwrap_or("");
    let params = match UrlSearchParams::new_with_str(search) {
        Ok(params) => params,
        Err(_) => return,
    };
    params.delete(name);
    let new_search_js = params.to_string();
    let new_search = new_search_js.as_string().unwrap_or_default();
    let new_url = if new_search.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{new_search}")
    };
    let _ = win
        .history()
        .and_then(|h| h.replace_state_with_url(&JsValue::NULL, "", Some(&new_url)));
}

pub fn get_local_storage_item(key: &str) -> Option<String> {
    let storage = window()?.local_storage().ok()??;
    storage.get_item(key).ok().flatten()
}

pub fn set_local_storage_item(key: &str, value: &str) {
    if let Some(storage) = window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(key, value);
    }
}

pub fn remove_local_storage_item(key: &str) {
    if let Some(storage) = window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.remove_item(key);
    }
}

pub async fn check_session(
    server_ip: &str,
    server_port: &str,
    token: &str,
) -> Result<(), String> {
    let url = build_http_url(server_ip, server_port, "users/me");
    let resp = send_request("GET", &url, Some(token), None).await?;
    if resp.status() == 401 {
        return Err("Session expired. Please log in again.".to_string());
    }
    if !resp.ok() {
        return Err(format!("Backend error (HTTP {}).", resp.status()));
    }
    Ok(())
}

pub async fn fetch_me(server_ip: &str, server_port: &str, token: &str) -> Result<User, String> {
    let url = build_http_url(server_ip, server_port, "users/me");
    get_json(&url, Some(token)).await
}
