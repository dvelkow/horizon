//! CPAL supplies the Web Audio stream. Browser autoplay policy requires a
//! gesture before its context can run; retain and remove those listeners here.
use wasm_bindgen::{JsCast, closure::Closure};

pub(super) struct Activation {
    context: web_sys::AudioContext,
    window: web_sys::Window,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

impl Activation {
    pub(super) fn new(stream: &cpal::Stream) -> Option<Self> {
        let cpal::platform::StreamInner::WebAudio(stream) = stream.as_inner();
        let context = stream.audio_context().clone();
        let window = web_sys::window()?;
        let resume = context.clone();
        let callback = Closure::wrap(Box::new(move |_: web_sys::Event| {
            if resume.state() == web_sys::AudioContextState::Suspended
                && let Ok(promise) = resume.resume()
            {
                wasm_bindgen_futures::spawn_local(async move {
                    if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                        log::debug!("Horizon: browser has not allowed audio playback yet");
                    }
                });
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        for event in ["pointerdown", "keydown", "touchend"] {
            let _ =
                window.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
        }
        Some(Self {
            context,
            window,
            callback,
        })
    }

    pub(super) fn state(&self) -> &'static str {
        match self.context.state() {
            web_sys::AudioContextState::Running => "running",
            web_sys::AudioContextState::Suspended => "waiting for a user gesture",
            web_sys::AudioContextState::Closed => "closed",
            _ => "unavailable",
        }
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        for event in ["pointerdown", "keydown", "touchend"] {
            let _ = self
                .window
                .remove_event_listener_with_callback(event, self.callback.as_ref().unchecked_ref());
        }
    }
}
