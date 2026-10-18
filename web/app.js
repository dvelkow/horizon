import {loadingStatus, startPlayerSkins} from "/bootstrap.js";
import {skinImage, drawCharacter} from "/wardrobe.js";
const local = location.hostname === "127.0.0.1";
const $ = id => document.getElementById(id);
let session;
let started = false;
let skins = [], selectedSkin = 'steve', editing = false;
let linkedSkin, lookingUp = false;
// The client release this page loaded (the gateway's /api/session names it).
let pageRelease = null;
const RESUME_KEY = "horizon-resume", RELOADED_KEY = "horizon-reloaded-for";
const stored = key => { try { return sessionStorage.getItem(key); } catch (_) { return null; } };
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
    linkedSkin = value.customSkin;
    $("linked-skin").hidden = !linkedSkin;
    $("minecraft-name").value = linkedSkin?.name ?? '';
    $("minecraft-skin-status").textContent = '';
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
  const skin = linkedSkin?.id === id ? linkedSkin : skins.find(s => s.id === id) ?? skins[0];
  if (!skin) return;
  $("skin-name").textContent = skin === linkedSkin ? `${skin.name}'s Minecraft skin` : skin.name;
  $("linked-skin").setAttribute('aria-pressed', String(skin === linkedSkin));
  for (const button of $("skin-options").children) { const active = button.dataset.skin === id; button.setAttribute('aria-checked', String(active)); button.tabIndex = active ? 0 : -1; }
  const image = await skinImage(skin);
  if (selectedSkin !== id) return;
  drawCharacter($("skin-preview"), image, false, skin.model); drawCharacter($("saved-skin"), image, false, skin.model);
}
async function previewMinecraftSkin() {
  if (lookingUp) return;
  const name = $("minecraft-name").value.trim();
  if (!/^[A-Za-z0-9_]{1,16}$/.test(name)) { $("minecraft-skin-status").textContent = 'Enter a Minecraft Java username (1–16 letters, numbers, or underscores).'; return; }
  lookingUp = true; $("preview-minecraft-skin").disabled = true; $("save-profile").disabled = true;
  $("minecraft-skin-status").textContent = 'Finding their skin…';
  try {
    const {skin} = await request('/api/skins/minecraft', {method:'POST',headers:{'Content-Type':'application/json','x-csrf-token':session.csrf},body:JSON.stringify({name})});
    await skinImage(skin); linkedSkin = skin; $("linked-skin").hidden = false;
    await selectSkin(skin.id);
    $("minecraft-skin-status").textContent = `Linked to ${skin.name}. Save below to use this skin.`;
  } catch(error) { $("minecraft-skin-status").textContent = error.message; }
  finally { lookingUp = false; $("preview-minecraft-skin").disabled = false; $("save-profile").disabled = false; }
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
/* A newer client went live since this page loaded: reload into it and rejoin, once per release. */
async function reloadedForNewRelease() {
  if (!pageRelease) return false;
  let latest;
  try { ({release: latest} = await request("/api/release")); } catch (_) { return false; }
  if (!latest || latest === pageRelease || stored(RELOADED_KEY) === latest) return false;
  try { sessionStorage.setItem(RELOADED_KEY, latest); sessionStorage.setItem(RESUME_KEY, String(Date.now())); } catch (_) { return false; }
  location.reload();
  return true;
}
async function play(join = true) {
  if (started) return;
  if (join && await reloadedForNewRelease()) return;
  if (!crossOriginIsolated) throw new Error("The host needs to enable the game's browser isolation settings.");
  if (!navigator.gpu) throw new Error("This browser does not offer WebGPU. Open this page in an updated Chrome or Edge browser.");
  if (join) await startPlayerSkins(session.name);
  globalThis.horizonOptions = Object.freeze({name: session?.name, join, release: pageRelease});
  started = true; $("launcher").hidden = true; $("horizon").hidden = false;
  // Sprinting is Ctrl and walking W: Ctrl+W closes a browser tab, and no page
  // can block it. In a world, the browser asks first ("Leave site?"); the
  // game's own reloads (a new release, the error screen) set horizonReloading.
  if (join) window.addEventListener("beforeunload", event => {
    if (globalThis.horizonReloading) return;
    event.preventDefault(); event.returnValue = "";
  });
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
$("preview-minecraft-skin").addEventListener('click', previewMinecraftSkin);
$("minecraft-name").addEventListener('keydown', event => { if(event.key==='Enter') { event.preventDefault(); previewMinecraftSkin(); } });
$("linked-skin").addEventListener('click', () => { if(linkedSkin) selectSkin(linkedSkin.id).catch(reportFailure); });
$("edit-profile").addEventListener('click', () => { editing = true; showSession(session); });
$("cancel-profile").addEventListener('click', () => { editing = false; showSession(session); });
$("play").addEventListener("click", () => play().catch(reportFailure));
$("logout").addEventListener("click", async () => {
  try { await request("/api/logout", {method: "POST", headers: {"x-csrf-token": session.csrf}}); editing = false; showSession({authenticated: false}); }
  catch (error) { reportFailure(error); }
});
try {
  const value = await request("/api/session");
  pageRelease = typeof value.release === "string" ? value.release : null;
  if (value.localPreview) await play(false); else { await loadWardrobe(); showSession(value); }
  // Reloaded for an update while playing: straight back into the world (a click recaptures the mouse).
  const resume = Number(stored(RESUME_KEY));
  try { sessionStorage.removeItem(RESUME_KEY); } catch (_) {}
  if (resume && Date.now() - resume < 120000 && value.authenticated && !value.needsProfile) await play();
  const auth = new URLSearchParams(location.search).get('auth');
  if (auth) { history.replaceState(null, '', '/'); showError(auth === 'cancelled' ? 'Discord sign-in was cancelled. Try again when you’re ready.' : 'Discord sign-in did not finish. Please try again.'); }
} catch (error) { $("connection-status").textContent = "The world is unavailable right now."; reportFailure(error); }
