//! Browser resource bootstrap for the shared in-memory filesystem.
//! The executable calls `ready().await` before starting the actual game.
use horizon_platform::fs;
use js_sys::Uint8Array;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    path::Path,
};
use wasm_bindgen::{JsCast, prelude::*};

#[wasm_bindgen(raw_module = "/bootstrap.js")]
extern "C" {
    #[wasm_bindgen(js_name = installGpuErrorHandling)]
    fn install_gpu_error_handling();
    #[wasm_bindgen(js_name = setPointerCapture)]
    fn set_pointer_capture(capture: bool) -> bool;
    #[wasm_bindgen(js_name = pointerCaptured)]
    pub fn pointer_captured() -> bool;
    #[wasm_bindgen(js_name = recordPerformance)]
    fn record_performance(fps: f32, frames: u64, world_frames: u64);
    #[wasm_bindgen(catch, js_name = loadResourceIndex)]
    async fn load_resource_index() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = fetchVerified)]
    async fn fetch_verified(url: &str, hash: &str, size: f64) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = loadingStatus)]
    fn loading_status(message: &str, percent: f64);
    #[wasm_bindgen(js_name = hideLoading)]
    pub fn hide_loading();
}

const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_RESOURCES: u64 = 1024 * 1024 * 1024;
fn fail(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}

/// Keeps startup failures visible instead of leaving a blank canvas.
pub fn startup_error(message: &str) {
    loading_status(&format!("Unable to start Horizon: {message}"), 0.0);
}

/// A lightweight browser-visible measurement for comparing native and web FPS.
pub fn report_performance(fps: f32, frames: u64, world_frames: u64) {
    record_performance(fps, frames, world_frames);
}
#[derive(Deserialize)]
struct ResourceIndex {
    schema: u32,
    file_count: usize,
    expanded_bytes: u64,
    bundles: Vec<Bundle>,
}
#[derive(Deserialize)]
struct Bundle {
    url: String,
    sha256: String,
    bytes: u64,
    expanded_bytes: u64,
    files: Vec<ResourceFile>,
}
#[derive(Deserialize)]
struct ResourceFile {
    path: String,
    bytes: u64,
    sha256: String,
}
fn json<T: serde::de::DeserializeOwned>(value: JsValue) -> Result<T, JsValue> {
    serde_json::from_str(
        &value
            .as_string()
            .ok_or_else(|| fail("Expected JSON text"))?,
    )
    .map_err(fail)
}
fn resource_path(value: &str) -> Result<(), JsValue> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains(['\\', '\0'])
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(fail("Unsafe prepared-resource path"));
    }
    Ok(())
}
fn verify(bytes: &[u8], expected: &str) -> Result<(), JsValue> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual != expected {
        return Err(fail("Resource checksum mismatch"));
    }
    Ok(())
}

/// Loads verified resources into the browser filesystem before the title starts.
/// Does not create a replacement title screen or start gameplay itself.
pub async fn ready() -> Result<(), JsValue> {
    install_gpu_error_handling();
    let args: Vec<String> = ["horizon-client", "--resources", "/resources", "--render-distance", "6"].into_iter().map(String::from).collect();
    horizon_platform::env::set_args(args);
    loading_status("Checking game resources…", 0.0);
    let index: ResourceIndex = json(load_resource_index().await?)?;
    if index.schema != 1
        || index.file_count > 100_000
        || index.expanded_bytes > MAX_RESOURCES
        || index.bundles.len() > 1024
    {
        return Err(fail("Unsupported or oversized resource index"));
    }
    let total = index
        .bundles
        .iter()
        .try_fold(0_u64, |sum, bundle| sum.checked_add(bundle.bytes))
        .ok_or_else(|| fail("Compressed resource size overflow"))?;
    if total > MAX_RESOURCES {
        return Err(fail("Compressed resources exceed browser budget"));
    }
    let mut downloaded = 0_u64;
    let mut names = BTreeSet::new();
    let mut expanded = 0_u64;
    for (position, bundle) in index.bundles.iter().enumerate() {
        if bundle.bytes > MAX_FILE
            || bundle.expanded_bytes > MAX_RESOURCES
            || bundle.files.len() > 100_000
        {
            return Err(fail("Resource bundle exceeds limits"));
        }
        loading_status(
            &format!(
                "Loading game resources ({}/{})…",
                position + 1,
                index.bundles.len()
            ),
            90.0 * downloaded as f64 / total.max(1) as f64,
        );
        let fetched = fetch_verified(&bundle.url, &bundle.sha256, bundle.bytes as f64).await?;
        let bytes = fetched
            .dyn_into::<Uint8Array>()
            .map_err(|_| fail("Resource response is not bytes"))?
            .to_vec();
        if bytes.len() as u64 != bundle.bytes {
            return Err(fail("Resource bundle size mismatch"));
        }
        verify(&bytes, &bundle.sha256)?;
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(fail)?;
        if zip.len() != bundle.files.len() {
            return Err(fail("Resource bundle entry count differs"));
        }
        let mut bundle_expanded = 0_u64;
        for (entry_index, descriptor) in bundle.files.iter().enumerate() {
            resource_path(&descriptor.path)?;
            if descriptor.bytes > MAX_FILE || !names.insert(descriptor.path.clone()) {
                return Err(fail("Oversized or duplicate prepared resource"));
            }
            let entry = zip.by_index(entry_index).map_err(fail)?;
            if entry.name() != descriptor.path
                || entry.size() != descriptor.bytes
                || entry.is_dir()
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(fail("Resource archive differs from its manifest"));
            }
            let mut bytes = Vec::new();
            entry
                .take(descriptor.bytes + 1)
                .read_to_end(&mut bytes)
                .map_err(fail)?;
            if bytes.len() as u64 != descriptor.bytes {
                return Err(fail("Expanded resource size mismatch"));
            }
            verify(&bytes, &descriptor.sha256)?;
            fs::preload(Path::new("/resources").join(&descriptor.path), bytes).map_err(fail)?;
            bundle_expanded = bundle_expanded
                .checked_add(descriptor.bytes)
                .ok_or_else(|| fail("Resource size overflow"))?;
        }
        if bundle_expanded != bundle.expanded_bytes {
            return Err(fail("Expanded bundle size mismatch"));
        }
        expanded = expanded
            .checked_add(bundle_expanded)
            .ok_or_else(|| fail("Resource size overflow"))?;
        if expanded > MAX_RESOURCES {
            return Err(fail("Resources exceed browser memory budget"));
        }
        downloaded += bundle.bytes;
    }
    if names.len() != index.file_count || expanded != index.expanded_bytes {
        return Err(fail("Incomplete game resources"));
    }
    fs::create_dir_all("/tmp").map_err(fail)?;
    loading_status("Starting Horizon…", 100.0);
    Ok(())
}

/// Request capture in a browser gesture and return the actual current state.
pub fn capture_pointer(capture: bool) -> bool { set_pointer_capture(capture) }
