/* Same-origin resource loading and browser-owned save persistence. No credentials. */
const CACHE = "horizon-resources-v1";
const MAX_FILE = 256 * 1024 * 1024;
const MAX_STATE = 16 * 1024 * 1024;
const MAX_SAVED_BYTES = 512 * 1024 * 1024;
const encoder = new TextEncoder();
let rootPromise;
let timerStarted = false;
let restorePending = false;
let releaseRestore;
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
export function persistenceWarning(message) {
  console.error("Horizon storage:", message);
  let warning = document.getElementById("horizon-storage-warning");
  if (!warning) {
    warning = document.createElement("div"); warning.id = "horizon-storage-warning";
    warning.setAttribute("role", "alert");
    Object.assign(warning.style, { position: "fixed", bottom: "12px", left: "12px", right: "12px", zIndex: "30",
      background: "#5a2b16", color: "white", padding: "12px", font: "14px system-ui" });
    document.body.append(warning);
  }
  warning.textContent = "Local changes are not saved yet: " + message;
}
export function persistenceHealthy() { document.getElementById("horizon-storage-warning")?.remove(); }

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
  let cache;
  try { cache = await caches.open(CACHE); } catch (_) { /* Network loading remains usable without cache. */ }
  const cached = await cache?.match(url);
  if (cached) {
    try {
      const bytes = await boundedResponse(cached, size);
      if (await sha256(bytes) === hash) return bytes;
    } catch (_) { /* Evict incomplete/corrupt cache entries and retry the network. */ }
    await cache.delete(url);
  }
  const response = await fetch(url, {credentials: "same-origin", cache: "no-store", redirect: "error"});
  const bytes = await boundedResponse(response, size);
  if (await sha256(bytes) !== hash) throw new Error("Resource hash mismatch");
  try { await cache?.put(url, new Response(bytes, {headers: {"Content-Type": "application/octet-stream"}})); }
  catch (_) { /* Cache quota does not invalidate verified resource bytes. */ }
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

function savePath(path) {
  if (typeof path !== "string" || !path.startsWith("/") || path === "/" || path.length > 4096
      || path.includes("\\") || path.includes("\0") || path.split("/").slice(1).some(p => !p || p === "." || p === "..")
      || path === "/resources" || path.startsWith("/resources/")) throw new Error("Invalid saved path");
  return path;
}
async function storageRoot() {
  if (!navigator.storage?.getDirectory || !navigator.locks?.request) throw new Error("This browser does not support Horizon's local save storage");
  if (!rootPromise) rootPromise = navigator.storage.getDirectory().then(root => root.getDirectoryHandle("horizon-v1", {create: true}));
  return rootPromise;
}
async function readState(root) {
  let handle;
  try { handle = await root.getFileHandle("state.json"); }
  catch (error) { if (error.name === "NotFoundError") return {schema: 1, revision: 0, entries: {}}; throw error; }
  const file = await handle.getFile();
  if (file.size > MAX_STATE) throw new Error("Saved file catalog is too large");
  const state = JSON.parse(await file.text());
  if (state.schema !== 1 || !Number.isSafeInteger(state.revision) || typeof state.entries !== "object" || !state.entries) throw new Error("Invalid saved file catalog");
  for (const [path, entry] of Object.entries(state.entries)) {
    savePath(path);
    if (entry.kind === "file") { checkedHash(entry.hash); checkedSize(entry.bytes); }
    else if (entry.kind !== "directory") throw new Error("Invalid saved entry");
  }
  return state;
}
async function atomicFile(root, name, bytes) {
  const handle = await root.getFileHandle(name, {create: true});
  const writable = await handle.createWritable();
  try { await writable.write(bytes); await writable.close(); }
  catch (error) { await writable.abort().catch(() => {}); throw error; }
}
export async function loadSavedIndex() {
  const root = await storageRoot();
  return navigator.locks.request("horizon-storage-v1", {mode: "shared"}, async () => JSON.stringify(await readState(root)));
}
// Hold this shared lock until Rust has read every blob in this catalog. Writers
// and their collector take the exclusive lock, so another tab cannot delete a
// snapshot's files halfway through restoring it. Context destruction releases
// browser locks automatically; normal success/error paths call finishRestore.
export async function beginRestore() {
  if (restorePending || releaseRestore) throw new Error("A local save restore is already running");
  restorePending = true;
  try {
    const root = await storageRoot();
    return await new Promise((resolve, reject) => {
      navigator.locks.request("horizon-storage-v1", {mode: "shared"}, async () => {
        const index = JSON.stringify(await readState(root));
        await new Promise(release => {
          releaseRestore = release; restorePending = false; resolve(index);
        });
      }).catch(error => { releaseRestore = undefined; reject(error); });
    });
  } finally { restorePending = false; }
}
export function finishRestore() {
  const release = releaseRestore; releaseRestore = undefined; release?.();
}
export async function loadSavedFile(hash, size) {
  checkedHash(hash); checkedSize(size);
  const root = await storageRoot();
  const blobs = await root.getDirectoryHandle("blobs");
  const file = await (await blobs.getFileHandle(hash)).getFile();
  if (file.size !== size) throw new Error("Saved file size mismatch");
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (await sha256(bytes) !== hash) throw new Error("Saved file checksum mismatch");
  return bytes;
}

async function collectObsolete(blobs, entries) {
  const referenced = new Set(Object.values(entries).filter(entry => entry.kind === "file").map(entry => entry.hash));
  for await (const [name, handle] of blobs.entries()) {
    if (handle.kind === "file" && /^[a-f0-9]{64}$/.test(name) && !referenced.has(name)) await blobs.removeEntry(name);
  }
}

// Immutable blobs are committed before the catalog. Closing state.json publishes
// the whole batch atomically; interrupted writes leave only unreferenced blobs.
// Failed batches are retried by Rust before later VFS changes are drained.
export async function persistBatch(changes) {
  if (!Array.isArray(changes) || changes.length > 100000) throw new Error("Invalid storage batch");
  const root = await storageRoot();
  await navigator.locks.request("horizon-storage-v1", async () => {
    const state = await readState(root);
    const entries = Object.assign(Object.create(null), state.entries);
    const blobs = await root.getDirectoryHandle("blobs", {create: true});
    // Clear orphaned writes from interrupted batches before allocating another
    // generation. The last committed catalog remains authoritative until close.
    await collectObsolete(blobs, state.entries);
    for (const change of changes) {
      const path = savePath(change.path);
      if (change.op === "remove") {
        for (const name of Object.keys(entries)) if (name === path || name.startsWith(path + "/")) delete entries[name];
      } else if (change.op === "directory") {
        entries[path] = {kind: "directory"};
      } else if (change.op === "write") {
        const bytes = change.bytes;
        if (!(bytes instanceof Uint8Array)) throw new Error("Invalid save bytes");
        checkedSize(bytes.byteLength);
        const hash = await sha256(bytes);
        let exists = false;
        try {
          const file = await (await blobs.getFileHandle(hash)).getFile();
          exists = file.size === bytes.byteLength && await sha256(await file.arrayBuffer()) === hash;
        } catch (error) { if (error.name !== "NotFoundError") throw error; }
        if (!exists) await atomicFile(blobs, hash, bytes);
        for (const name of Object.keys(entries)) if (name.startsWith(path + "/")) delete entries[name];
        entries[path] = {kind: "file", hash, bytes: bytes.byteLength};
      } else throw new Error("Unknown save operation");
    }
    let savedBytes = 0;
    for (const entry of Object.values(entries)) if (entry.kind === "file") savedBytes += entry.bytes;
    if (savedBytes > MAX_SAVED_BYTES) throw new Error("Local saves exceed the 512 MB browser limit");
    const encoded = encoder.encode(JSON.stringify({schema: 1, revision: state.revision + 1, entries}));
    if (encoded.byteLength > MAX_STATE) throw new Error("Saved catalog exceeds storage limit");
    await atomicFile(root, "state.json", encoded);
    // Publication succeeded. Cleanup failure cannot roll the commit back; the
    // next checkpoint retries collection before doing any new writes.
    try { await collectObsolete(blobs, entries); }
    catch (error) { console.warn("Horizon will retry old-save cleanup:", error?.message ?? error); }
  });
}
export function schedulePersistence(callback) {
  if (timerStarted) return;
  timerStarted = true;
  setInterval(callback, 2000);
  document.addEventListener("visibilitychange", () => { if (document.hidden) callback(); });
  // Browsers may terminate a page before async I/O completes; regular checkpoints
  // are authoritative. pagehide is only a best effort, not a durability promise.
  addEventListener("pagehide", callback);
}
