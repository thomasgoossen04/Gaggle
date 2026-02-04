use wasm_bindgen::{prelude::JsValue, JsCast};
use wasm_bindgen_futures::JsFuture;
use web_sys::{window, Headers, Request, RequestInit, Response, UrlSearchParams};
use yew::UseStateHandle;

use crate::app::AppState;

pub const SERVER_IP_KEY: &str = "gaggle_server_ip";
pub const SESSION_TOKEN_KEY: &str = "gaggle_session_token";
pub const LOGIN_SUCCESS_KEY: &str = "gaggle_login_success";

pub fn handle_login(server_ip: &str) {
    let server_ip = server_ip.trim();
    if server_ip.is_empty() {
        return;
    }

    set_local_storage_item(SERVER_IP_KEY, server_ip);

    let redirect = current_app_url();
    let redirect = js_sys::encode_uri_component(&redirect)
        .as_string()
        .unwrap_or_default();
    let login_url = format!("http://{server_ip}:2121/auth/discord/login?redirect={redirect}");

    if let Some(win) = window() {
        if win.location().set_href(&login_url).is_ok() {
            return;
        }
        let _ = win.open_with_url_and_target(&login_url, "_blank");
    }
}

pub fn handle_logout(app_state: UseStateHandle<AppState>) {
    remove_local_storage_item(SESSION_TOKEN_KEY);
    app_state.set(AppState {
        logged_in: false,
        server_ip: get_local_storage_item(SERVER_IP_KEY),
        session_token: None,
        auth_error: None,
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

pub async fn check_session(server_ip: &str, token: &str) -> Result<(), String> {
    let url = format!("http://{server_ip}:2121/users/me");
    let mut opts = RequestInit::new();
    opts.method("GET");

    let headers = Headers::new().map_err(|_| "Failed to create headers.".to_string())?;
    headers
        .set("Authorization", &format!("Bearer {token}"))
        .map_err(|_| "Failed to set auth header.".to_string())?;
    opts.headers(&headers);

    let request =
        Request::new_with_str_and_init(&url, &opts).map_err(|_| "Failed to build request.".to_string())?;
    let win = window().ok_or_else(|| "No window available.".to_string())?;
    let resp_value = JsFuture::from(win.fetch_with_request(&request))
        .await
        .map_err(|_| format!("Cannot reach backend at {url}."))?;
    let resp: Response = resp_value
        .dyn_into()
        .map_err(|_| "Invalid response type.".to_string())?;

    if resp.status() == 401 {
        return Err("Session expired. Please log in again.".to_string());
    }
    if !resp.ok() {
        return Err(format!("Backend error (HTTP {}).", resp.status()));
    }

    Ok(())
}
