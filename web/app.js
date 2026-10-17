import {loadingStatus, startPlayerSkins} from "/bootstrap.js";
import {skinImage, drawCharacter} from "/wardrobe.js";
const local = location.hostname === "127.0.0.1";
const $ = id => document.getElementById(id);
let session;
let started = false;
let skins = [], selectedSkin = 'steve', editing = false;
function showError(message) { $("entry-error").textContent = message; $("entry-error").hidden = false; }
function reportFailure(error) {
  const message = error?.message ?? String(error);
  if (local) window.horizonLastError = error?.stack ?? message;
  if (started) loadingStatus("Unable to start (dev)Modcraft: " + message, 0); else showError(message);
}
window.addEventListener("error", event => reportFailure(event.error ?? new Error(event.message)));
window.addEventListener("unhandledrejection", event => reportFailure(event.reason));
if (local) {
  Error.stackTraceLimit = 100;
  window.horizonLogs = [];
  for (const kind of ["error", "warn", "info"]) {
    const original = console[kind].bind(console);
    console[kind] = (...args) => { window.horizonLogs.push(args.map(String).join(" ")); if (window.horizonLogs.length > 200) window.horizonLogs.shift(); original(...args); };
  }
}
async function request(path, options = {}) {
  const response = await fetch(path, {credentials: "same-origin", cache: "no-store", redirect: "error", ...options});
  if (response.status === 404 && local && path === "/api/session") return {localPreview: true};
  const data = await response.json();
  if (!response.ok) throw new Error(data.error ?? "The world could not be reached. Try again shortly.");
  return data;
}
function showSession(value) {
  session = value; $("connection-status").hidden = true; $("entry-error").hidden = true;
  $("discord-login").hidden = value.authenticated === true;
  $("profile-form").hidden = !value.authenticated || (!value.needsProfile && !editing);
  $("signed-in").hidden = !value.authenticated || value.needsProfile || editing;
  $("logout").hidden = !value.authenticated;
  $("session-name").textContent = value.name ?? "";
  if (value.authenticated) {
    $("player-name").value = value.name ?? (value.displayName ?? '').replace(/[^A-Za-z0-9_]/g, '').slice(0, 16);
    $("player-name").readOnly = !!value.name;
    $("profile-title").textContent = value.name ? 'A fresh look.' : 'Make yourself at home.';
    $("save-profile").textContent = value.name ? 'Save skin' : "That's me — let's play";
    $("cancel-profile").hidden = !value.name;
    selectSkin(value.skin ?? 'steve').catch(reportFailure);
  }
}
async function selectSkin(id) {
  selectedSkin = id;
  const skin = skins.find(s => s.id === id) ?? skins[0];
  if (!skin) return;
  $("skin-name").textContent = skin.name;
  for (const button of $("skin-options").children) { const active = button.dataset.skin === id; button.setAttribute('aria-checked', String(active)); button.tabIndex = active ? 0 : -1; }
  const image = await skinImage(skin);
  if (selectedSkin !== id) return;
  drawCharacter($("skin-preview"), image); drawCharacter($("saved-skin"), image);
}
async function loadWardrobe() {
  ({skins} = await request('/api/skins'));
  for (const skin of skins) {
    const button = document.createElement('button'); button.type = 'button'; button.className = 'skin-option'; button.dataset.skin = skin.id;
    button.setAttribute('role', 'radio'); button.setAttribute('aria-label', skin.name); button.setAttribute('aria-checked', 'false'); button.title = skin.name;
    const face = document.createElement('canvas'); face.width = face.height = 32; face.setAttribute('aria-hidden', 'true'); button.append(face);
    button.addEventListener('click', () => selectSkin(skin.id).catch(reportFailure));
    button.addEventListener('keydown', event => { if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) { event.preventDefault(); cycleSkin(['ArrowLeft', 'ArrowUp'].includes(event.key) ? -1 : 1); $("skin-options").querySelector('[aria-checked=true]').focus(); } });
    $("skin-options").append(button);
    skinImage(skin).then(image => drawCharacter(face, image, true)).catch(reportFailure);
  }
}
function cycleSkin(direction) { const i = skins.findIndex(s => s.id === selectedSkin); selectSkin(skins[(i + direction + skins.length) % skins.length].id).catch(reportFailure); }
async function play(join = true) {
  if (started) return;
  if (!crossOriginIsolated) throw new Error("The host needs to enable the game's browser isolation settings.");
  if (!navigator.gpu) throw new Error("This browser does not offer WebGPU. Open this page in an updated Chrome or Edge browser.");
  if (join) await startPlayerSkins(session.name);
  globalThis.horizonOptions = Object.freeze({name: session?.name, join});
  started = true; $("launcher").hidden = true; $("horizon").hidden = false;
  loadingStatus("Opening your world…", 0);
  if (join) { const {startMods} = await import("/mods-runtime/host.js"); await startMods(); }
  const {default: init} = await import("/pkg/horizon_client.js");
  await init();
}
$("profile-form").addEventListener("submit", async event => {
  event.preventDefault(); const button = $("save-profile"); button.disabled = true; $("entry-error").hidden = true;
  try {
    const first = session.needsProfile;
    const updated = await request("/api/profile", {method: "POST", headers: {"Content-Type": "application/json", "x-csrf-token": session.csrf}, body: JSON.stringify({name: $("player-name").value.trim(), skin: selectedSkin})});
    editing = false; showSession(updated);
    if (first) await play();
  } catch (error) { reportFailure(error); } finally { button.disabled = false; }
});
$("skin-prev").addEventListener('click', () => cycleSkin(-1));
$("skin-next").addEventListener('click', () => cycleSkin(1));
$("edit-profile").addEventListener('click', () => { editing = true; showSession(session); });
$("cancel-profile").addEventListener('click', () => { editing = false; showSession(session); });
$("play").addEventListener("click", () => play().catch(reportFailure));
$("logout").addEventListener("click", async () => {
  try { await request("/api/logout", {method: "POST", headers: {"x-csrf-token": session.csrf}}); editing = false; showSession({authenticated: false}); }
  catch (error) { reportFailure(error); }
});
try {
  const value = await request("/api/session");
  if (value.localPreview) await play(false); else { await loadWardrobe(); showSession(value); }
  const auth = new URLSearchParams(location.search).get('auth');
  if (auth) { history.replaceState(null, '', '/'); showError(auth === 'cancelled' ? 'Discord sign-in was cancelled. Try again when you’re ready.' : 'Discord sign-in did not finish. Please try again.'); }
} catch (error) { $("connection-status").textContent = "The world is unavailable right now."; reportFailure(error); }
