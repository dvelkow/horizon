//! Clipboard reads are promises. Synchronous fields use the last known text;
//! focus, paste events and explicit reads refresh that cache asynchronously.
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[derive(Default)]
struct Cache {
    text: RefCell<String>,
    reading: Cell<bool>,
    revision: Cell<u64>,
}

fn refresh(cache: Rc<Cache>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    if cache.reading.replace(true) {
        return;
    }
    let revision = cache.revision.get();
    let promise = window.navigator().clipboard().read_text();
    spawn_local(async move {
        if let Ok(value) = JsFuture::from(promise).await
            && let Some(text) = value.as_string()
            && cache.revision.get() == revision
        {
            *cache.text.borrow_mut() = text;
        }
        // Denied clipboard permission leaves in-game copy/paste working.
        cache.reading.set(false);
    });
}

pub(super) struct ClipboardCache {
    cache: Rc<Cache>,
    window: Option<web_sys::Window>,
    paste: Closure<dyn FnMut(web_sys::ClipboardEvent)>,
    focus: Closure<dyn FnMut(web_sys::Event)>,
}

impl Default for ClipboardCache {
    fn default() -> Self {
        let cache = Rc::new(Cache::default());
        let pasted = cache.clone();
        let paste = Closure::wrap(Box::new(move |event: web_sys::ClipboardEvent| {
            if let Some(data) = event.clipboard_data()
                && let Ok(text) = data.get_data("text/plain")
            {
                pasted.revision.set(pasted.revision.get().wrapping_add(1));
                *pasted.text.borrow_mut() = text;
            }
        }) as Box<dyn FnMut(web_sys::ClipboardEvent)>);
        let focused = cache.clone();
        let focus = Closure::wrap(Box::new(move |_: web_sys::Event| {
            refresh(focused.clone());
        }) as Box<dyn FnMut(web_sys::Event)>);
        let window = web_sys::window();
        if let Some(window) = &window {
            let _ =
                window.add_event_listener_with_callback("paste", paste.as_ref().unchecked_ref());
            let _ =
                window.add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref());
        }
        Self {
            cache,
            window,
            paste,
            focus,
        }
    }
}

impl ClipboardCache {
    pub(super) fn read_async(&self) -> impl std::future::Future<Output = String> + 'static {
        let cache = self.cache.clone();
        // Start the browser API within the caller's user-gesture handler.
        let promise = self
            .window
            .as_ref()
            .map(|window| window.navigator().clipboard().read_text());
        let revision = cache.revision.get();
        async move {
            if let Some(promise) = promise
                && let Ok(value) = JsFuture::from(promise).await
                && let Some(text) = value.as_string()
            {
                if cache.revision.get() == revision {
                    *cache.text.borrow_mut() = text.clone();
                }
                return text;
            }
            cache.text.borrow().clone()
        }
    }
    pub(super) fn accept_read(&self, text: String) {
        self.cache
            .revision
            .set(self.cache.revision.get().wrapping_add(1));
        *self.cache.text.borrow_mut() = text;
    }
    pub(super) fn refresh(&self) {
        refresh(self.cache.clone());
    }
    pub(super) fn text(&self) -> String {
        self.cache.text.borrow().clone()
    }
    pub(super) fn write(&self, text: &str) {
        self.cache
            .revision
            .set(self.cache.revision.get().wrapping_add(1));
        *self.cache.text.borrow_mut() = text.to_owned();
        if let Some(window) = &self.window {
            let promise = window.navigator().clipboard().write_text(text);
            spawn_local(async move {
                if JsFuture::from(promise).await.is_err() {
                    log::debug!("Horizon: system clipboard write denied; text kept in game");
                }
            });
        }
    }
}

impl Drop for ClipboardCache {
    fn drop(&mut self) {
        if let Some(window) = &self.window {
            let _ = window
                .remove_event_listener_with_callback("paste", self.paste.as_ref().unchecked_ref());
            let _ = window
                .remove_event_listener_with_callback("focus", self.focus.as_ref().unchecked_ref());
        }
    }
}
