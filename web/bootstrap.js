/* Same-origin resource loading for the browser title. */
const MAX_FILE = 256 * 1024 * 1024;
const MAX_STATE = 16 * 1024 * 1024;
let gpuFailure;
let gpuGuardInstalled = false;
let pointerHandlersInstalled = false;
let pointerWanted = false;
let pointerPending = false;
let pointerGesture = 0;
let requestedPointerGesture = -1;

function pointerHint(show) {
  let hint = document.getElementById("horizon-pointer-hint");
  if (!show) { hint?.remove(); return; }
  if (hint) return;
  hint = document.createElement("div"); hint.id = "horizon-pointer-hint";
  hint.setAttribute("role", "status"); hint.textContent = "Click the game to look around. Press Esc to release the mouse.";
  Object.assign(hint.style, {position: "fixed", bottom: "24px", left: "50%", transform: "translateX(-50%)", zIndex: "15",
    background: "#1b2d37e8", color: "white", padding: "12px 18px", borderRadius: "4px", font: "14px system-ui",
    pointerEvents: "none", maxWidth: "calc(100vw - 32px)", textAlign: "center"});
  document.body.append(hint);
}
export function pointerCaptured() {
  const canvas = document.getElementById("horizon");
  return !!canvas && document.pointerLockElement === canvas;
}
function pointerChanged() {
  globalThis.horizonPointerCaptured = pointerCaptured();
  pointerHint(pointerWanted && !globalThis.horizonPointerCaptured);
}
export function setPointerCapture(capture) {
  if (!pointerHandlersInstalled) {
    pointerHandlersInstalled = true;
    document.addEventListener("pointerlockchange", pointerChanged);
    document.addEventListener("pointerlockerror", () => { pointerPending = false; pointerChanged(); });
    // One request per real canvas gesture prevents retry loops after Esc or a
    // browser refusal. This capture listener runs before winit's input handler.
    document.addEventListener("pointerdown", event => {
      if (event.isTrusted && event.target === document.getElementById("horizon")) pointerGesture++;
    }, true);
  }
  pointerWanted = capture === true;
  const canvas = document.getElementById("horizon");
  if (!pointerWanted) {
    if (pointerCaptured()) document.exitPointerLock();
    pointerHint(false); return false;
  }
  if (pointerCaptured()) { pointerChanged(); return true; }
  pointerHint(true);
  if (canvas && navigator.userActivation?.isActive && !pointerPending && requestedPointerGesture !== pointerGesture) {
    requestedPointerGesture = pointerGesture; pointerPending = true;
    try {
      Promise.resolve(canvas.requestPointerLock()).catch(() => {
        // A denied lock is an ordinary interaction state, never a game failure.
      }).finally(() => {
        pointerPending = false;
        if (!pointerWanted && pointerCaptured()) document.exitPointerLock();
        pointerChanged();
      });
    } catch (_) { pointerPending = false; pointerChanged(); }
  }
  // requestPointerLock is asynchronous. Report the actual browser state only.
  return pointerCaptured();
}

// One update per second from the renderer; no per-frame logging or DOM work.
export function recordPerformance(fps, frames, worldFrames) {
  globalThis.horizonPerformance = {fps: Number(fps), frames: Number(frames), worldFrames: Number(worldFrames), lastTimestamp: performance.now()};
}

// Attach before wgpu creates a device. WebGPU validation failures can otherwise
// surface only in the browser's GPU-process log and leave an empty canvas.
export function installGpuErrorHandling() {
  if (gpuGuardInstalled || typeof GPUAdapter === "undefined") return;
  gpuGuardInstalled = true;
  const requestDevice = GPUAdapter.prototype.requestDevice;
  const requestFrame = requestAnimationFrame.bind(globalThis);
  const cancelFrame = cancelAnimationFrame.bind(globalThis);
  const frames = new Set();
  globalThis.requestAnimationFrame = callback => {
    if (gpuFailure) return 0;
    const id = requestFrame(time => { frames.delete(id); if (!gpuFailure) callback(time); });
    frames.add(id); return id;
  };
  globalThis.cancelAnimationFrame = id => { frames.delete(id); cancelFrame(id); };
  const stop = (device, error) => {
    if (gpuFailure) return;
    gpuFailure = String(error?.message ?? error ?? "Unknown graphics error").slice(0, 4096);
    for (const id of frames) cancelFrame(id);
    frames.clear();
    loadingStatus("Graphics stopped: " + gpuFailure, 0);
    const box = document.getElementById("horizon-loading");
    box.setAttribute("role", "alert");
    box.querySelector("progress").hidden = true;
    const reload = document.createElement("button"); reload.textContent = "Reload Horizon";
    reload.addEventListener("click", () => location.reload()); box.append(reload);
    console.error("Horizon graphics stopped:", gpuFailure);
    // Do not keep submitting invalid command buffers after a fatal error.
    device.destroy();
  };
  GPUAdapter.prototype.requestDevice = async function(...args) {
    const device = await requestDevice.apply(this, args);
    device.addEventListener("uncapturederror", event => stop(device, event.error));
    device.lost.then(info => { if (info.reason !== "destroyed") stop(device, info.message || "The graphics device was lost"); });
    const createShader = device.createShaderModule.bind(device);
    device.createShaderModule = descriptor => {
      const shader = createShader(descriptor);
      // Some Chrome/Dawn builds report compilation only through this query,
      // without dispatching an uncapturederror event to the page.
      shader.getCompilationInfo().then(info => {
        const errors = info.messages.filter(message => message.type === "error");
        if (errors.length) stop(device, errors.slice(0, 4).map(error => `Shader ${descriptor.label || "compilation"}, line ${error.lineNum}: ${error.message}`).join("\n"));
      }).catch(error => stop(device, error));
      return shader;
    };
    return device;
  };
}

// The launcher supplies the authenticated session name before Rust starts.
export function browserOptions() {
  const value = globalThis.horizonOptions;
  return JSON.stringify(value && typeof value === "object"
    ? {name: value.name, join: value.join === true} : {join: false});
}

export function loadingStatus(message, percent) {
  if (gpuFailure) { message = "Graphics stopped: " + gpuFailure; percent = 0; }
  let box = document.getElementById("horizon-loading");
  if (!box) {
    box = document.createElement("div");
    box.id = "horizon-loading";
    box.setAttribute("role", "status");
    Object.assign(box.style, { position: "fixed", inset: "0", zIndex: "20", background: "#0d171e",
      color: "#edf7f5", display: "grid", placeContent: "center", font: "16px system-ui", textAlign: "center", padding: "24px" });
    const title = document.createElement("strong"); title.textContent = "Horizon"; title.style.fontSize = "32px";
    const label = document.createElement("p"); label.dataset.loadingMessage = "";
    const progress = document.createElement("progress"); progress.max = 100; progress.style.width = "min(360px,75vw)";
    box.append(title, label, progress); document.body.append(box);
  }
  box.querySelector("[data-loading-message]").textContent = message;
  box.querySelector("progress").value = Math.max(0, Math.min(100, percent));
}

export function hideLoading() { if (!gpuFailure) document.getElementById("horizon-loading")?.remove(); }
function checkedHash(hash) {
  if (typeof hash !== "string" || !/^[a-f0-9]{64}$/.test(hash)) throw new Error("Invalid resource hash");
  return hash;
}
async function sha256(bytes) {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), b => b.toString(16).padStart(2, "0")).join("");
}
function checkedSize(size, maximum = MAX_FILE) {
  if (!Number.isSafeInteger(size) || size < 0 || size > maximum) throw new Error("Invalid resource size");
  return size;
}
function assetUrl(value) {
  const url = new URL(value, location.origin);
  if (url.origin !== location.origin || !/^\/assets\/[a-f0-9]{64}\.(json|zip)$/.test(url.pathname)
      || url.search || url.hash || url.username || url.password) throw new Error("Unexpected resource URL");
  return url.href;
}
async function boundedResponse(response, expected) {
  if (!response.ok || response.type === "opaque") throw new Error(`Resource request failed (${response.status})`);
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Resource response has no body");
  const output = new Uint8Array(expected);
  let count = 0;
  try {
    for (;;) {
      const {done, value} = await reader.read();
      if (done) break;
      if (count + value.byteLength > expected) throw new Error("Resource exceeds declared size");
      output.set(value, count); count += value.byteLength;
    }
  } catch (error) { await reader.cancel().catch(() => {}); throw error; }
  if (count !== expected) throw new Error("Resource size mismatch");
  return output;
}
export async function fetchVerified(value, hash, size) {
  const url = assetUrl(value); checkedHash(hash); checkedSize(size);
  if (!new URL(url).pathname.includes(hash)) throw new Error("Resource URL/hash mismatch");
  const response = await fetch(url, {credentials: "same-origin", cache: "no-store", redirect: "error"});
  const bytes = await boundedResponse(response, size);
  if (await sha256(bytes) !== hash) throw new Error("Resource hash mismatch");
  return bytes;
}
export async function loadResourceIndex() {
  if (!isSecureContext || !crypto.subtle) throw new Error("Horizon needs HTTPS for verified browser resources");
  const response = await fetch("/assets/index.json", {credentials: "same-origin", cache: "no-store", redirect: "error"});
  if (!response.ok) throw new Error(`Resource index unavailable (${response.status})`);
  const reader = response.body?.getReader();
  if (!reader) throw new Error("Resource pointer has no body");
  const buffer = new Uint8Array(4096); let length = 0;
  for (;;) {
    const {done, value} = await reader.read(); if (done) break;
    if (length + value.byteLength > buffer.length) { await reader.cancel(); throw new Error("Resource pointer exceeds size limit"); }
    buffer.set(value, length); length += value.byteLength;
  }
  const text = new TextDecoder("utf-8", {fatal: true}).decode(buffer.subarray(0, length));
  const pointer = JSON.parse(text);
  if (pointer.schema !== 1) throw new Error("Unsupported resource index schema");
  checkedSize(pointer.bytes, MAX_STATE);
  return new TextDecoder("utf-8", {fatal: true}).decode(await fetchVerified(pointer.url, pointer.sha256, pointer.bytes));
}
