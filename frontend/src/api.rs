use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, Response};

pub async fn get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    token: Option<&str>,
) -> Result<T, String> {
    let response = send_request("GET", url, token, None).await?;
    if response.status() == 403 {
        return Err("Admin access required.".to_string());
    }
    if response.status() == 401 {
        return Err("Session expired. Please log in again.".to_string());
    }
    if !response.ok() {
        return Err(format!("Request failed (HTTP {}).", response.status()));
    }
    let json = JsFuture::from(
        response
            .json()
            .map_err(|_| "Failed to parse JSON.".to_string())?,
    )
    .await
    .map_err(|_| "Failed to parse JSON.".to_string())?;
    let data: T = serde_wasm_bindgen::from_value(json)
        .map_err(|_| "Invalid response.".to_string())?;
    Ok(data)
}

pub async fn send_json(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> Result<Response, String> {
    let body_str = body.map(|b| b.to_string());
    send_request(method, url, token, body_str.as_deref()).await
}

pub async fn send_request(
    method: &str,
    url: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Result<Response, String> {
    let opts = RequestInit::new();
    opts.set_method(method);

    let headers = Headers::new().map_err(|_| "Failed to create headers.".to_string())?;
    if let Some(token) = token {
        headers
            .set("Authorization", &format!("Bearer {token}"))
            .map_err(|_| "Failed to set auth header.".to_string())?;
    }
    if body.is_some() {
        headers
            .set("Content-Type", "application/json")
            .map_err(|_| "Failed to set content type.".to_string())?;
    }
    opts.set_headers(&headers);

    if let Some(body) = body {
        opts.set_body(&wasm_bindgen::JsValue::from_str(body));
    }

    let request = Request::new_with_str_and_init(url, &opts)
        .map_err(|_| "Failed to build request.".to_string())?;
    let win = web_sys::window().ok_or_else(|| "No window available.".to_string())?;
    let resp_value = JsFuture::from(win.fetch_with_request(&request))
        .await
        .map_err(|_| format!("Cannot reach backend at {url}."))?;
    let resp: Response = resp_value
        .dyn_into()
        .map_err(|_| "Invalid response type.".to_string())?;
    Ok(resp)
}
