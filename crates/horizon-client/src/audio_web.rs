//! CPAL supplies the Web Audio stream. Browser autoplay policy requires a
//! gesture before its context can run; retain and remove those listeners here.
pub(super) use crate::sound_events::{SoundAsset, sound_index};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::JsFuture;

pub(super) fn context(stream: &cpal::Stream) -> web_sys::AudioContext {
    let cpal::platform::StreamInner::WebAudio(stream) = stream.as_inner();
    stream.audio_context().clone()
}

/// Let the browser decode Ogg off the Rust event-loop turn. The existing mixer
/// still owns gain, positional attenuation, looping, and channel lifetime.
pub(super) async fn decode(
    context: &web_sys::AudioContext,
    bytes: Vec<u8>,
) -> Option<(Vec<f32>, u32)> {
    let encoded = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
    let buffer = JsFuture::from(context.decode_audio_data(&encoded).ok()?)
        .await
        .ok()?
        .dyn_into::<web_sys::AudioBuffer>()
        .ok()?;
    let length = buffer.length() as usize;
    let channels = buffer.number_of_channels();
    if channels == 0 || channels > 8 || !buffer.sample_rate().is_finite() {
        return None;
    }
    // Some official music lasts fifteen minutes. Downsample long tracks to
    // keep retained mono PCM within the 64 MiB cache budget rather than
    // rejecting those tracks or copying every stereo sample twice.
    let stride = length
        .div_ceil((64 << 20) / std::mem::size_of::<f32>())
        .max(1);
    let mut mono = vec![0.0; length.div_ceil(stride)];
    let mut scratch = [0.0; 4096];
    // Web Audio rejects SharedArrayBuffer-backed views. Rust memory is shared
    // with workers, so use a browser-owned array at this API boundary.
    let browser_scratch = js_sys::Float32Array::new_with_length(scratch.len() as u32);
    let gain = 1.0 / (channels as f32 * stride as f32);
    for start in (0..length).step_by(scratch.len()) {
        let count = scratch.len().min(length - start);
        for channel in 0..channels {
            buffer
                .copy_from_channel_with_f32_array_and_start_in_channel(
                    &browser_scratch,
                    channel as i32,
                    start as u32,
                )
                .ok()?;
            browser_scratch.copy_to(&mut scratch);
            for (offset, sample) in scratch[..count].iter().enumerate() {
                mono[(start + offset) / stride] += sample * gain;
            }
        }
        if start != 0 && start % (1 << 20) == 0 {
            // Permit input and rendering while downmixing a long music track.
            let window = web_sys::window()?;
            let pause = js_sys::Promise::new(&mut |resolve, _| {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
            });
            JsFuture::from(pause).await.ok()?;
        }
    }
    Some((mono, (buffer.sample_rate() / stride as f32) as u32))
}

pub(super) struct Activation {
    context: web_sys::AudioContext,
    window: web_sys::Window,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

impl Activation {
    pub(super) fn new(stream: &cpal::Stream) -> Option<Self> {
        let context = context(stream);
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
