//! Platform services. Native consumers use the standard library unchanged;
//! browser consumers preload immutable resources and persist mutable files.
pub use std::*;

pub mod time {
    pub use web_time::*;
}

#[cfg(not(target_arch = "wasm32"))]
pub use std::{env, fs, sync, thread};

#[cfg(target_arch = "wasm32")]
pub mod env;
#[cfg(target_arch = "wasm32")]
pub mod fs;
#[cfg(target_arch = "wasm32")]
pub mod thread;

#[cfg(target_arch = "wasm32")]
pub mod sync;
// Exercise the browser lock implementation without replacing native APIs.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "sync.rs"]
mod browser_sync_tests;

/// Same-origin URL of a prepared resource, including optional audio files.
pub fn resource_url(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let relative = text.strip_prefix("/resources/").unwrap_or(&text);
    format!("/assets/{}", relative.trim_start_matches('/'))
}

/// Filesystem queries routed to the browser's preloaded filesystem on WASM.
/// `std::path::Path` itself remains portable; its OS-backed query methods do not.
pub trait PathExt {
    fn platform_exists(&self) -> bool;
    fn platform_is_file(&self) -> bool;
    fn platform_is_dir(&self) -> bool;
}
impl<T: AsRef<std::path::Path> + ?Sized> PathExt for T {
    fn platform_exists(&self) -> bool {
        fs::metadata(self.as_ref()).is_ok()
    }
    fn platform_is_file(&self) -> bool {
        fs::metadata(self.as_ref()).is_ok_and(|m| m.is_file())
    }
    fn platform_is_dir(&self) -> bool {
        fs::metadata(self.as_ref()).is_ok_and(|m| m.is_dir())
    }
}

/// Bridge a browser wall-clock value into crates using the standard time type.
pub fn system_time(value: time::SystemTime) -> std::time::SystemTime {
    #[cfg(not(target_arch = "wasm32"))]
    {
        value
    }
    #[cfg(target_arch = "wasm32")]
    {
        match value.duration_since(time::UNIX_EPOCH) {
            Ok(d) => std::time::UNIX_EPOCH + d,
            Err(e) => std::time::UNIX_EPOCH - e.duration(),
        }
    }
}
