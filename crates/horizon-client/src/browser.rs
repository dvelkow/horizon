//! Browser resource bootstrap and asynchronous persistence for the shared VFS.
//! The executable calls `ready().await` before starting the actual game.
use horizon_platform::fs::{self, Change};
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
    path::Path,
};
use wasm_bindgen::{JsCast, prelude::*};

#[wasm_bindgen(raw_module = "/bootstrap.js")]
extern "C" {
    #[wasm_bindgen(js_name = installGpuErrorHandling)]
    fn install_gpu_error_handling();
    #[wasm_bindgen(js_name = recordPerformance)]
    fn record_performance(fps: f32, frames: u64, world_frames: u64);
    #[wasm_bindgen(js_name = setPointerCapture)]
    fn set_pointer_capture(capture: bool) -> bool;
    #[wasm_bindgen(js_name = pointerCaptured)]
    pub fn pointer_captured() -> bool;
    #[wasm_bindgen(js_name = takePointerPauseRequest)]
    pub fn take_pointer_pause_request() -> bool;
    #[wasm_bindgen(js_name = takeFullscreenExit)]
    pub fn take_fullscreen_exit() -> bool;
    #[wasm_bindgen(js_name = openModPortal)]
    pub fn open_mod_portal();
    #[wasm_bindgen(catch, js_name = browserOptions)]
    fn browser_options() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = selectedPlayerSkin)]
    fn selected_player_skin(uuid: &str) -> String;
    #[wasm_bindgen(js_name = requestPlayerSkin)]
    fn request_player_skin(path: &str);
    #[wasm_bindgen(js_name = takePlayerSkinTextures)]
    fn take_player_skin_textures() -> Array;
    #[wasm_bindgen(js_name = takeHostedContent)]
    fn take_hosted_content() -> Array;
    #[wasm_bindgen(catch, js_name = loadResourceIndex)]
    async fn load_resource_index() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = fetchVerified)]
    async fn fetch_verified(url: &str, hash: &str, size: f64) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = beginRestore)]
    async fn load_saved_index() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = finishRestore)]
    fn finish_restore();
    #[wasm_bindgen(catch, js_name = loadSavedFile)]
    async fn load_saved_file(hash: &str, size: f64) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch, js_name = persistBatch)]
    async fn persist_batch(changes: &Array) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = loadingStatus)]
    fn loading_status(message: &str, percent: f64);
    #[wasm_bindgen(js_name = persistenceWarning)]
    fn persistence_warning(message: &str);
    #[wasm_bindgen(js_name = persistenceHealthy)]
    fn persistence_healthy();
    #[wasm_bindgen(js_name = hideLoading)]
    pub fn hide_loading();
    #[wasm_bindgen(js_name = schedulePersistence)]
    fn schedule_persistence(callback: &js_sys::Function);
}

thread_local! {
    static READY_PLAYER_SKINS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}
fn imported_skin_hash(path: &str) -> Option<&str> {
    let value = path.strip_prefix("modcraft/skins/")?;
    let (model, hash) = value.split_once('/')?;
    (matches!(model, "wide" | "slim")
        && hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
    .then_some(hash)
}
pub fn player_skin_ready(path: &str, ready: bool) {
    READY_PLAYER_SKINS.with_borrow_mut(|skins| {
        if ready {
            skins.insert(path.to_owned());
        } else {
            skins.remove(path);
        }
    });
}
pub fn take_player_skins() -> Vec<(String, Vec<u8>)> {
    take_player_skin_textures()
        .iter()
        .take(32)
        .filter_map(|entry| {
            let path = Reflect::get(&entry, &"path".into()).ok()?.as_string()?;
            let hash = imported_skin_hash(&path)?;
            let data = Reflect::get(&entry, &"bytes".into())
                .ok()?
                .dyn_into::<Uint8Array>()
                .ok()?;
            if data.length() > 64 * 1024 {
                return None;
            }
            let bytes = data.to_vec();
            if format!("{:x}", Sha256::digest(&bytes)) != hash
                || bytes.get(..8)? != b"\x89PNG\r\n\x1a\n"
                || bytes.get(16..24)? != [0, 0, 0, 64, 0, 0, 0, 64]
            {
                return None;
            }
            let (width, height, rgba) = crate::hud::decode_bytes(&bytes).ok()?;
            (width == 64 && height == 64).then_some((path, rgba))
        })
        .collect()
}
/// Only prepared skins or verified, bounded textures on our own host.
pub fn player_skin(uuid: Option<u128>) -> Option<String> {
    let key = uuid
        .map(|value| format!("{value:032x}"))
        .unwrap_or_default();
    let path = selected_player_skin(&key);
    if imported_skin_hash(&path).is_some() {
        if READY_PLAYER_SKINS.with_borrow(|skins| skins.contains(&path)) {
            return Some(path);
        }
        request_player_skin(&path);
        return None;
    }
    let name = path.strip_prefix("entity/player/wide/")?;
    [
        "steve", "alex", "ari", "efe", "kai", "makena", "noor", "sunny", "zuri",
    ]
    .contains(&name)
    .then_some(path)
}

const MAX_FILE: u64 = 128 * 1024 * 1024;
const MAX_RESOURCES: u64 = 1024 * 1024 * 1024;
/// Where the hosted world's content mods go: under the immutable resources,
/// so they are never saved with the player's files and a mod the host
/// removed is gone at the next start.
const HOSTED_MODS: &str = "/resources/.hosted-mods";
/// The hosted world's content packages, each and in all (web/mods/host.js).
const MAX_CONTENT_FILE: usize = 64 << 20;
const MAX_CONTENT: usize = 64 << 20;
fn fail(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}

/// Releases the browser's shared snapshot lock on every normal success/error
/// path, including failures while decoding or verifying an individual file.
struct RestoreGuard;
impl Drop for RestoreGuard {
    fn drop(&mut self) {
        finish_restore();
    }
}

/// Keeps startup failures visible instead of leaving a blank canvas.
pub fn startup_error(message: &str) {
    loading_status(
        &format!("Unable to start {}: {message}", crate::brand::NAME),
        0.0,
    );
}

/// A lightweight browser-visible measurement for comparing native and web FPS.
pub fn report_performance(fps: f32, frames: u64, world_frames: u64) {
    record_performance(fps, frames, world_frames);
}

/// Requests or restores mouse control and reports actual browser capture state.
pub fn capture_pointer(capture: bool) -> bool {
    set_pointer_capture(capture)
}
#[derive(Deserialize)]
struct ResourceIndex {
    schema: u32,
    file_count: usize,
    expanded_bytes: u64,
    bundles: Vec<Bundle>,
}
#[derive(Deserialize)]
struct BrowserOptions {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    join: bool,
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
#[derive(Deserialize)]
struct SavedIndex {
    schema: u32,
    entries: BTreeMap<String, SavedEntry>,
}
#[derive(Deserialize)]
struct SavedEntry {
    kind: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    bytes: u64,
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
/// The hosted world's content mods, as the mods host fetched and verified
/// them before the game started: checked again here (ids, sizes, SHA-256)
/// and put in [`HOSTED_MODS`]. The game layers exactly these, by id, as the
/// world's server does (game/mods.rs `content_root`). False with none.
fn hosted_content() -> Result<bool, JsValue> {
    let field = |entry: &JsValue, name: &str| Reflect::get(entry, &JsValue::from_str(name));
    let mut ids = BTreeSet::new();
    let mut total = 0_usize;
    for entry in take_hosted_content().iter() {
        let id = field(&entry, "id")?
            .as_string()
            .filter(|id| {
                (1..=64).contains(&id.len())
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            .ok_or_else(|| fail("Invalid content mod id"))?;
        let hash = field(&entry, "sha256")?
            .as_string()
            .ok_or_else(|| fail("Content mod without a checksum"))?;
        let bytes = field(&entry, "bytes")?
            .dyn_into::<Uint8Array>()
            .map_err(|_| fail("Content mod is not bytes"))?;
        let size = bytes.length() as usize;
        total = total.saturating_add(size);
        if !ids.insert(id.clone()) || size > MAX_CONTENT_FILE || total > MAX_CONTENT {
            return Err(fail("Duplicate or oversized content mods"));
        }
        let bytes = bytes.to_vec();
        verify(&bytes, &hash)?;
        fs::preload(Path::new(HOSTED_MODS).join(format!("{id}.hmod")), bytes).map_err(fail)?;
    }
    Ok(!ids.is_empty())
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

/// Loads verified resource bundles, then restores the browser's mutable files.
/// Does not create a replacement title screen or start gameplay itself.
pub async fn ready() -> Result<(), JsValue> {
    install_gpu_error_handling();
    let options: BrowserOptions = json(browser_options()?)?;
    let mut args: Vec<String> = [
        "horizon-client",
        "--resources",
        "/resources",
        "--render-distance",
        "6",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if options.join {
        let name = options
            .name
            .ok_or_else(|| fail("Sign in before joining the world"))?;
        if name.is_empty()
            || name.len() > 16
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(fail(
                "Player names need 1–16 letters, numbers, or underscores",
            ));
        }
        args.extend([
            "--server".into(),
            "gateway".into(),
            "--hosted".into(),
            "--name".into(),
            name,
        ]);
        // The world's blocks and items before anything loads: the server's
        // chunks name block states by these ids.
        if hosted_content()? {
            args.extend(["--approved-mods".into(), HOSTED_MODS.into()]);
        }
    }
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
    loading_status("Restoring local settings and saves…", 93.0);
    let saved_json = load_saved_index().await?;
    let restore_guard = RestoreGuard;
    let saved: SavedIndex = json(saved_json)?;
    if saved.schema != 1 || saved.entries.len() > 100_000 {
        return Err(fail("Invalid saved-file index"));
    }
    let saved_total = saved
        .entries
        .values()
        .try_fold(0_u64, |sum, entry| sum.checked_add(entry.bytes))
        .ok_or_else(|| fail("Saved-file size overflow"))?;
    if saved_total > 512 * 1024 * 1024 {
        return Err(fail("Local saves exceed the browser memory budget"));
    }
    for (path, entry) in saved.entries {
        if !path.starts_with('/')
            || path == "/"
            || path.starts_with("/resources/")
            || path == "/resources"
        {
            return Err(fail("Invalid saved path"));
        }
        resource_path(path.trim_start_matches('/'))?;
        match entry.kind.as_str() {
            "directory" => fs::create_dir_all(&path).map_err(fail)?,
            "file" => {
                let bytes = load_saved_file(&entry.hash, entry.bytes as f64)
                    .await?
                    .dyn_into::<Uint8Array>()
                    .map_err(|_| fail("Saved file response is not bytes"))?
                    .to_vec();
                if bytes.len() as u64 != entry.bytes {
                    return Err(fail("Saved file size mismatch"));
                }
                verify(&bytes, &entry.hash)?;
                fs::preload(&path, bytes).map_err(fail)?;
            }
            _ => return Err(fail("Invalid saved file kind")),
        }
    }
    drop(restore_guard);
    // Directory creation during restoration is already persisted in the catalog.
    fs::drain_changes().map_err(fail)?;
    fs::create_dir_all("/tmp").map_err(fail)?;
    loading_status(&format!("Starting {}…", crate::brand::NAME), 100.0);
    Ok(())
}

thread_local! {
    static FLUSHING: Cell<bool> = const { Cell::new(false) };
    static PENDING: RefCell<Option<Vec<Change>>> = const { RefCell::new(None) };
    static STARTED: Cell<bool> = const { Cell::new(false) };
}
fn field(object: &Object, name: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(object, &JsValue::from_str(name), value).map(|_| ())
}
fn batch(changes: &[Change]) -> Result<Array, JsValue> {
    let values = Array::new();
    for change in changes {
        let value = Object::new();
        let (path, operation) = match change {
            Change::Write { path, bytes } => {
                field(&value, "bytes", &Uint8Array::from(bytes.as_slice()))?;
                (path, "write")
            }
            Change::CreateDir { path } => (path, "directory"),
            Change::Remove { path } => (path, "remove"),
        };
        field(&value, "path", &JsValue::from_str(&path.to_string_lossy()))?;
        field(&value, "op", &JsValue::from_str(operation))?;
        values.push(&value);
    }
    Ok(values)
}
/// Commits the oldest pending batch; failed writes remain pending for retry.
pub async fn flush_persistence() -> Result<(), JsValue> {
    if FLUSHING.replace(true) {
        return Err(fail("A local-storage checkpoint is already running"));
    }
    let result = async {
        let changes = match PENDING.with(|p| p.borrow_mut().take()) {
            Some(pending) => pending,
            None => fs::drain_changes().map_err(fail)?,
        };
        if changes.is_empty() {
            return Ok(());
        }
        let result = match batch(&changes) {
            Ok(values) => persist_batch(&values).await.map(|_| ()),
            Err(error) => Err(error),
        };
        if result.is_err() {
            PENDING.with(|p| *p.borrow_mut() = Some(changes));
        }
        result
    }
    .await;
    FLUSHING.set(false);
    result
}
/// Starts serialized periodic checkpoints. Start once after `ready` succeeds.
pub fn start_persistence() {
    if STARTED.replace(true) {
        return;
    }
    let callback = Closure::<dyn FnMut()>::new(|| {
        if FLUSHING.get() {
            return;
        }
        wasm_bindgen_futures::spawn_local(async {
            match flush_persistence().await {
                Ok(()) => persistence_healthy(),
                Err(error) => {
                    persistence_warning(&error.as_string().unwrap_or_else(|| format!("{error:?}")))
                }
            }
        });
    });
    schedule_persistence(callback.as_ref().unchecked_ref());
    callback.forget();
}
