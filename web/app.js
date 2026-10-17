import {loadingStatus} from "/bootstrap.js";
const local = location.hostname === "127.0.0.1";
const $ = id => document.getElementById(id);
let session;
let started = false;
function showError(message) { $("entry-error").textContent = message; $("entry-error").hidden = false; }
function reportFailure(error) {
  const message = error?.message ?? String(error);
  if (local) window.horizonLastError = error?.stack ?? message;
  if (started) loadingStatus("Unable to start Horizon: " + message, 0); else showError(message);
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
  $("login-form").hidden = value.authenticated === true; $("signed-in").hidden = value.authenticated !== true;
  $("session-name").textContent = value.name ?? "";
}
async function play(join = true) {
  if (started) return;
  if (!crossOriginIsolated) throw new Error("The host needs to enable the game's browser isolation settings.");
  if (!navigator.gpu) throw new Error("This browser does not offer WebGPU. Open this page in an updated Chrome or Edge browser.");
  globalThis.horizonOptions = Object.freeze({name: session?.name, join});
  started = true; $("launcher").hidden = true; $("horizon").hidden = false;
  loadingStatus("Opening your world…", 0);
  if (join) { const {startMods} = await import("/mods-runtime/host.js"); await startMods(); }
  const {default: init} = await import("/pkg/horizon_client.js");
  await init();
}
$("login-form").addEventListener("submit", async event => {
  event.preventDefault(); const button = event.currentTarget.querySelector("button"); button.disabled = true; $("entry-error").hidden = true;
  const invite = $("invite").value; $("invite").value = "";
  try {
    showSession(await request("/api/login", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify({invite, name: $("player-name").value.trim()})}));
    await play();
  } catch (error) { reportFailure(error); } finally { button.disabled = false; }
});
$("play").addEventListener("click", () => play().catch(reportFailure));
$("logout").addEventListener("click", async () => {
  try { await request("/api/logout", {method: "POST", headers: {"x-csrf-token": session.csrf}}); showSession({authenticated: false}); }
  catch (error) { reportFailure(error); }
});
try {
  const value = await request("/api/session");
  if (value.localPreview) await play(false); else showSession(value);
} catch (error) { $("connection-status").textContent = "The world is unavailable right now."; reportFailure(error); }
