//! Browser fetch transport. Authentication uses only the official HTTPS
//! discovery service; neither credentials nor request bodies are logged.
use super::{AuthSession, JoinError};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;

struct Timeout {
    window: web_sys::Window,
    id: i32,
    _callback: Closure<dyn FnMut()>,
}
impl Drop for Timeout {
    fn drop(&mut self) {
        self.window.clear_timeout_with_handle(self.id);
    }
}

async fn request(
    url: &str,
    body: Option<&str>,
    maximum: usize,
    same_origin: bool,
) -> Result<(u16, Vec<u8>), JoinError> {
    let unavailable = |_| JoinError::Unavailable;
    let window = web_sys::window().ok_or(JoinError::Unavailable)?;
    let controller = web_sys::AbortController::new().map_err(unavailable)?;
    let abort = controller.clone();
    let callback = Closure::wrap(Box::new(move || abort.abort()) as Box<dyn FnMut()>);
    let id = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            15_000,
        )
        .map_err(unavailable)?;
    let _timeout = Timeout {
        window: window.clone(),
        id,
        _callback: callback,
    };
    let init = web_sys::RequestInit::new();
    init.set_signal(Some(&controller.signal()));
    init.set_credentials(if same_origin {
        web_sys::RequestCredentials::SameOrigin
    } else {
        web_sys::RequestCredentials::Omit
    });
    init.set_redirect(web_sys::RequestRedirect::Error);
    init.set_mode(if same_origin {
        web_sys::RequestMode::SameOrigin
    } else {
        web_sys::RequestMode::Cors
    });
    if let Some(body) = body {
        init.set_method("POST");
        init.set_body(&JsValue::from_str(body));
    }
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(unavailable)?;
    if body.is_some() {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(unavailable)?;
    }
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(unavailable)?
        .dyn_into::<web_sys::Response>()
        .map_err(unavailable)?;
    let status = response.status();
    let bytes = response_bytes(&response, maximum)
        .await
        .inspect_err(|_| controller.abort())?;
    Ok((status, bytes))
}

async fn response_bytes(
    response: &web_sys::Response,
    maximum: usize,
) -> Result<Vec<u8>, JoinError> {
    let unavailable = |_| JoinError::Unavailable;
    if response
        .headers()
        .get("Content-Length")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|size| size > maximum)
    {
        return Err(JoinError::Unavailable);
    }
    let Some(stream) = response.body() else {
        return Ok(Vec::new());
    };
    let reader = web_sys::ReadableStreamDefaultReader::new(&stream).map_err(unavailable)?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(unavailable)?;
        if js_sys::Reflect::get(&chunk, &JsValue::from_str("done"))
            .map_err(unavailable)?
            .as_bool()
            == Some(true)
        {
            break;
        }
        let value =
            js_sys::Reflect::get(&chunk, &JsValue::from_str("value")).map_err(unavailable)?;
        let chunk = js_sys::Uint8Array::new(&value);
        if chunk.length() as usize > maximum.saturating_sub(bytes.len()) {
            let _ = reader.cancel();
            reader.release_lock();
            return Err(JoinError::Unavailable);
        }
        bytes.extend(chunk.to_vec());
    }
    reader.release_lock();
    Ok(bytes)
}

/// Bounded resource downloads; a CORS or network refusal returns no resource.
pub(crate) async fn get_bytes(url: &str, maximum: usize) -> Option<Vec<u8>> {
    let (status, bytes) = request(url, None, maximum, false).await.ok()?;
    (200..300).contains(&status).then_some(bytes)
}

/// Authenticated, same-origin game audio only. The hash comes from the pinned
/// startup resource manifest, and is checked again even for a disk-cache hit.
pub(crate) async fn get_resource_bytes(url: &str, size: usize, sha256: &str) -> Option<Vec<u8>> {
    use sha2::{Digest, Sha256};
    if !url.starts_with("/assets/")
        || size > 32 << 20
        || sha256.len() != 64
        || !sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }
    let window = web_sys::window()?;
    let url = web_sys::Url::new_with_base(url, &window.location().href().ok()?).ok()?;
    if url.origin() != window.location().origin().ok()? {
        return None;
    }
    url.set_search(&format!("sha256={sha256}"));
    let key = url.href();
    let valid =
        |bytes: &[u8]| bytes.len() == size && format!("{:x}", Sha256::digest(bytes)) == sha256;
    // Storage may be unavailable or full (including private browsing). Network
    // playback still works; cache failures never become silent audio failures.
    let cache = match window.caches().ok() {
        Some(storage) => JsFuture::from(storage.open("horizon-audio-v1"))
            .await
            .ok()
            .and_then(|value| value.dyn_into::<web_sys::Cache>().ok()),
        None => None,
    };
    if let Some(cache) = &cache
        && let Ok(value) = JsFuture::from(cache.match_with_str(&key)).await
        && let Ok(response) = value.dyn_into::<web_sys::Response>()
    {
        if let Ok(bytes) = response_bytes(&response, size).await
            && valid(&bytes)
        {
            return Some(bytes);
        }
        let _ = JsFuture::from(cache.delete_with_str(&key)).await;
    }
    let (status, bytes) = request(&key, None, size, true).await.ok()?;
    if !(200..300).contains(&status) || !valid(&bytes) {
        return None;
    }
    if let Some(cache) = cache {
        trim_audio_cache(&cache, bytes.len()).await;
        // A Response body cannot use the worker-shared Rust memory directly.
        let copy = js_sys::Uint8Array::from(bytes.as_slice());
        if let Ok(response) = web_sys::Response::new_with_opt_js_u8_array(Some(&copy)) {
            let _ = response
                .headers()
                .set("Content-Length", &bytes.len().to_string());
            let _ = response.headers().set("Content-Type", "audio/ogg");
            let _ = JsFuture::from(cache.put_with_str(&key, &response)).await;
        }
    }
    Some(bytes)
}

/// FIFO eviction keeps the optional encoded sound cache near 64 MiB. Browser
/// quota eviction is also safe because every entry can be downloaded again.
async fn trim_audio_cache(cache: &web_sys::Cache, incoming: usize) {
    let Ok(keys) = JsFuture::from(cache.keys()).await else {
        return;
    };
    let mut entries = Vec::new();
    let mut total = incoming;
    for value in js_sys::Array::from(&keys).iter() {
        let Ok(request) = value.dyn_into::<web_sys::Request>() else {
            continue;
        };
        let size = match JsFuture::from(cache.match_with_request(&request)).await {
            Ok(value) => value
                .dyn_into::<web_sys::Response>()
                .ok()
                .and_then(|response| response.headers().get("Content-Length").ok().flatten())
                .and_then(|value| value.parse::<usize>().ok()),
            Err(_) => None,
        };
        if let Some(size) = size {
            total = total.saturating_add(size);
            entries.push((request, size));
        } else {
            let _ = JsFuture::from(cache.delete_with_request(&request)).await;
        }
    }
    for (request, size) in entries {
        if total <= 64 << 20 {
            break;
        }
        let _ = JsFuture::from(cache.delete_with_request(&request)).await;
        total = total.saturating_sub(size);
    }
}

/// Complete the same online-mode join as native, without blocking the UI.
/// The official services must permit CORS; this never sends tokens to a proxy.
pub async fn join_server(session: &AuthSession, server_hash: &str) -> Result<(), JoinError> {
    let bytes = get_bytes(super::DEFAULT_DISCOVERY, 1 << 20)
        .await
        .ok_or(JoinError::Unavailable)?;
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| JoinError::Unavailable)?;
    let endpoint = super::endpoint(&document, "session", "join").ok_or(JoinError::Unavailable)?;
    let url = web_sys::Url::new(&endpoint).map_err(|_| JoinError::Unavailable)?;
    if url.protocol() != "https:"
        || !matches!(
            url.hostname().as_str(),
            "api.minecraftservices.com" | "sessionserver.mojang.com"
        )
        || !url.username().is_empty()
        || !url.password().is_empty()
    {
        return Err(JoinError::Unavailable);
    }
    let (status, bytes) = request(
        &endpoint,
        Some(&super::join_request(session, server_hash)),
        1 << 20,
        false,
    )
    .await?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(JoinError::from_response(
            status,
            &String::from_utf8_lossy(&bytes),
        ))
    }
}
