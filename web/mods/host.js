/* Approved live components. This manager never executes guest code on the UI thread. */
const MAX_MODS = 32;
const MAX_BROWSER_MODS = 8;
const MAX_FILES = 128;
const MAX_BUNDLE = 64 * 1024 * 1024;
/* Every content mod's package together, as the portal allows (web/server/mod-store.mjs). */
const MAX_CONTENT = 64 * 1024 * 1024;
const MAX_OUTPUT = 64;
const MAX_QUEUE = 32;
const LIMITS = {initialize: 15000, tick: 1000, frame: 1000, chat: 1000, command: 1500};
/* The host's choices for approved mods: commands each runs quietly once the
   player is in the world, before its panel shows. Infinity Stones' gauntlet
   panel starts hidden; /infinity hud shows it (the mod keeps no settings). */
const STARTUP_COMMANDS = {infinity_stones: ["infinity hud"]};
const clip = (value, maximum = 256) => Array.from(String(value).slice(0, maximum * 2)).slice(0, maximum).join("");
const hex = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const errorText = error => clip(error?.message ?? error);
/* A newer game client went live: the notice, then a reload once the player stands still. */
const RELOAD_NOTICE_MS = 10000;
const RELOAD_SETTLE_MS = 20000;
const RESUME_KEY = "horizon-resume";
const RELOADED_KEY = "horizon-reloaded-for";
const allowedCapabilities = new Set(["game-read", "chat", "hud", "movement", "registries"]);

function modUrl(value, hash) {
  const url = new URL(value, location.origin);
  if (url.origin !== location.origin || !url.pathname.startsWith(`/mods/bundles/${hash}/`) || url.search || url.hash
      || url.username || url.password) throw new Error("Unexpected mod file URL");
  return url.href;
}
function packageUrl(value, hash) {
  const url = new URL(value, location.origin);
  if (url.origin !== location.origin || url.pathname !== `/mods/packages/${hash}.hmod` || url.search || url.hash
      || url.username || url.password) throw new Error("Unexpected mod package URL");
  return url.href;
}
async function sha256(bytes) {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), b => b.toString(16).padStart(2, "0")).join("");
}
function pathName(path) {
  if (typeof path !== "string" || !/^[a-zA-Z0-9_.\/-]+$/.test(path) || path.startsWith("/")
      || path.split("/").some(p => !p || p === "." || p === "..")) throw new Error("Unsafe mod file path");
  return path;
}
function manifest(entry) {
  const m = entry.manifest;
  if (!m || !/^[a-z0-9_]{1,64}$/.test(m.id) || entry.id !== m.id || !["0.1", "0.2"].includes(m.api)
      || !Array.isArray(m.capabilities) || m.capabilities.some(c => typeof c !== "string" || !/^[a-z][a-z-]{0,31}$/.test(c))) throw new Error("Invalid approved mod manifest");
  if (typeof m.name !== "string" || !m.name || m.name.length > 128 || typeof m.version !== "string" || m.version.length > 64) throw new Error("Invalid approved mod identity");
  if (!hex(entry.sha256)) throw new Error("Invalid approved mod hash");
  const side = m.side ?? "client";
  if (!["client", "server", "both"].includes(side)) throw new Error("Invalid mod side");
  if (entry.capabilities && JSON.stringify([...new Set(entry.capabilities)].sort()) !== JSON.stringify([...new Set(m.capabilities)].sort())) throw new Error("Published capability lists differ");
  return {id: m.id, name: m.name, version: m.version, api: m.api, side,
    description: clip(m.description ?? "", 4096), authors: Array.isArray(m.authors) ? m.authors.slice(0, 16).map(a => clip(a, 128)) : [],
    capabilities: [...new Set(m.capabilities)]};
}
async function boundedFetch(url, limit) {
  const abort = new AbortController(); const timeout = setTimeout(() => abort.abort(), 30000);
  try {
    const response = await fetch(url, {credentials: "same-origin", cache: "no-store", redirect: "error", signal: abort.signal});
    if (!response.ok) throw new Error(`Mod request failed (${response.status})`);
    const reader = response.body?.getReader(); if (!reader) throw new Error("Mod response has no body");
    const pieces = []; let count = 0;
    for (;;) {
      const {done, value} = await reader.read(); if (done) break;
      count += value.byteLength;
      if (count > limit) { await reader.cancel(); throw new Error("Mod response exceeds size limit"); }
      pieces.push(value);
    }
    const bytes = new Uint8Array(count); let offset = 0;
    for (const piece of pieces) { bytes.set(piece, offset); offset += piece.byteLength; }
    return bytes;
  } finally { clearTimeout(timeout); }
}
async function verifiedFiles(entry) {
  if (!Array.isArray(entry.files) || !entry.files.length || entry.files.length > MAX_FILES) throw new Error("Invalid mod bundle files");
  const seen = new Set(); const files = []; let total = 0; let entryPath;
  const entryUrl = modUrl(entry.entry, entry.sha256);
  for (const file of entry.files) {
    const path = pathName(file.path);
    if (seen.has(path) || !hex(file.sha256) || !Number.isSafeInteger(file.size) || file.size < 0) throw new Error("Invalid mod artifact");
    seen.add(path); total += file.size;
    if (total > MAX_BUNDLE) throw new Error("Mod bundle exceeds browser memory budget");
    const url = modUrl(file.url, entry.sha256);
    const bytes = await boundedFetch(url, file.size);
    if (bytes.byteLength !== file.size) throw new Error("Mod file size mismatch");
    if (await sha256(bytes) !== file.sha256) throw new Error("Mod file checksum mismatch");
    if (url === entryUrl) entryPath = path;
    files.push({path, bytes: bytes.buffer});
  }
  if (!entryPath?.endsWith(".js")) throw new Error("Missing verified JavaScript instantiator");
  return {entryPath, files};
}
async function fetchIndex() {
  const bytes = await boundedFetch("/mods/index.json", 2 * 1024 * 1024);
  const index = JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes));
  if (index.schema !== 1 || !Array.isArray(index.mods) || index.mods.length > MAX_MODS || !["number", "string"].includes(typeof index.revision)) throw new Error("Invalid approved-mod index");
  return index;
}
const isContent = manifest => manifest.capabilities.includes("registries");
/* The world's content mods (capability registries): their packages, verified, by id. The game
   layers exactly these before it starts, in this order, as the world's server does, so the
   block-state ids in the server's chunks mean the same blocks here. */
async function contentPackages(index) {
  const rows = index.mods.map(entry => ({entry, manifest: manifest(entry)})).filter(row => isContent(row.manifest))
    .sort((a, b) => a.manifest.id < b.manifest.id ? -1 : a.manifest.id > b.manifest.id ? 1 : 0);
  if (new Set(rows.map(row => row.manifest.id)).size !== rows.length) throw new Error("Duplicate content mod identifier");
  const packages = []; let total = 0;
  for (const {entry, manifest} of rows) {
    if (!Number.isSafeInteger(entry.size) || entry.size <= 0) throw new Error(`${manifest.name}: invalid package size`);
    total += entry.size;
    if (total > MAX_CONTENT) throw new Error("The world's content mods exceed 64 MB");
    const bytes = await boundedFetch(packageUrl(entry.url, entry.sha256), entry.size);
    if (bytes.byteLength !== entry.size || await sha256(bytes) !== entry.sha256) throw new Error(`${manifest.name}: package checksum mismatch`);
    packages.push({id: manifest.id, sha256: entry.sha256, bytes});
  }
  return packages;
}
/* A Mods-page row with nothing to run in this page. */
function placeholder(row, status, error = "") {
  return {manifest: row.manifest, hash: row.entry.sha256, status, error, panel: null, hooks: {commands: []}, stop() {}, enqueue() { return false; }, runStartup() {}};
}
function boundedPanel(panel) {
  if (panel === null || panel === undefined) return null;
  if (!["top-left", "top-right", "bottom-left", "bottom-right"].includes(panel.corner) || !Array.isArray(panel.lines)) throw new Error("Invalid mod HUD panel");
  return {corner: panel.corner, lines: panel.lines.slice(0, 16).map(line => ({text: clip(line.text), color: Number(line.color) >>> 0}))};
}

class Instance {
  constructor(manager, entry, normalized) {
    this.manager = manager; this.entry = entry; this.manifest = normalized; this.hash = entry.sha256;
    this.status = "loading"; this.error = ""; this.panel = null; this.hooks = {commands: []};
    this.queue = []; this.busy = null; this.serial = 0; this.worker = null; this.deadline = null; this.committed = false;
    this.initialOutput = [];
    this.logWindow = 0; this.logCount = 0;
    this.startup = [...(STARTUP_COMMANDS[normalized.id] ?? [])];
  }
  /* The startup commands, once there is a player for them to act on. */
  runStartup() {
    if (!this.startup.length || this.status !== "running" || !this.manager.snapshot.player) return;
    for (const line of this.startup.splice(0)) {
      const name = line.split(" ", 1)[0];
      if (this.hooks.commands.includes(name)) this.enqueue("command", {name, args: line.slice(name.length).trimStart()}, true);
    }
  }
  async initialize() {
    const verified = await verifiedFiles(this.entry);
    if (this.manager.closed) throw new Error("Mod host stopped");
    return new Promise((resolve, reject) => {
      this.resolve = resolve; this.reject = reject;
      this.worker = new Worker("/mods-runtime/worker.js", {type: "module", name: `horizon-mod-${this.manifest.id}`});
      this.worker.onmessage = event => this.receive(event.data);
      this.worker.onerror = event => { event.preventDefault(); this.fail("Worker failed: " + event.message); };
      this.worker.onmessageerror = () => this.fail("Invalid Worker message");
      this.deadline = setTimeout(() => this.fail("Initialization exceeded its time budget"), LIMITS.initialize);
      this.worker.postMessage({kind: "initialize", manifest: this.manifest, snapshot: this.manager.snapshot,
        entry: verified.entryPath, files: verified.files}, verified.files.map(file => file.bytes));
    });
  }
  receive(message) {
    try {
      if (this.status === "failed" || this.status === "off") return;
      if (message.kind === "failed") { this.fail(message.reason); return; }
      if (message.kind === "ready" && this.status === "loading") {
        clearTimeout(this.deadline);
        const hooks = message.hooks;
        if (!hooks || !Array.isArray(hooks.commands)) throw new Error("Invalid mod registration");
        this.hooks = {commands: [...new Set(hooks.commands.filter(c => typeof c === "string" && /^[a-z][a-z0-9_:-]{0,63}$/.test(c)))].slice(0, 128),
          tick: hooks.tick === true, frame: hooks.frame === true, chat: hooks.chat === true};
        this.panel = boundedPanel(message.panel); this.initialOutput = this.sanitizeOutput(message.out);
        this.status = "running"; this.resolve(this); return;
      }
      if (message.kind !== "result" || !this.busy || message.call !== this.busy.call) throw new Error("Unexpected mod response");
      clearTimeout(this.deadline); const quiet = this.busy.quiet; this.busy = null;
      this.panel = boundedPanel(message.panel);
      const out = this.sanitizeOutput(message.out);
      if (!quiet) this.manager.addOutput(out);
      if (this.manifest.capabilities.includes("movement")) this.manager.addNudge(message.nudge);
      this.pump();
    } catch (error) { this.fail(errorText(error)); }
  }
  sanitizeOutput(rows) {
    if (!Array.isArray(rows)) throw new Error("Invalid mod output");
    const result = [];
    for (const row of rows.slice(0, 32)) {
      if (row.kind === "chat" && this.manifest.capabilities.includes("chat")) result.push({kind: "chat", name: this.manifest.name, text: clip(row.text)});
      else if (row.kind === "feedback") result.push({kind: "feedback", text: clip(row.text), error: row.error === true});
      else if (row.kind === "log") {
        const now = performance.now();
        if (now - this.logWindow >= 1000) { this.logWindow = now; this.logCount = 0; }
        if (++this.logCount <= 16) console.info(`[mod ${this.manifest.id}] ${clip(row.text)}`);
      }
    }
    return result.slice(0, 8);
  }
  commit() {
    this.committed = true; this.manager.addOutput(this.initialOutput); this.initialOutput = [];
  }
  enqueue(method, payload, quiet = false) {
    if (this.status !== "running") return false;
    if (method !== "command" && !this.hooks[method]) return false;
    if (method === "tick" || method === "frame") {
      const existing = this.queue.find(item => item.method === method);
      if (existing) { existing.payload = payload; return true; }
    }
    if (this.queue.length >= MAX_QUEUE) {
      if (method === "command") this.manager.addOutput([{kind: "feedback", text: `${this.manifest.name} is busy; try again shortly`, error: true}]);
      return false;
    }
    this.queue.push({method, payload, quiet}); this.pump(); return true;
  }
  pump() {
    if (this.busy || this.status !== "running" || !this.queue.length) return;
    const item = this.queue.shift(); const call = ++this.serial;
    this.busy = {call, method: item.method, quiet: item.quiet === true};
    this.deadline = setTimeout(() => this.fail(`${item.method} exceeded its time budget`), LIMITS[item.method]);
    this.worker.postMessage({kind: "call", call, method: item.method, payload: item.payload, snapshot: this.manager.snapshot});
  }
  fail(reason) {
    if (this.status === "failed" || this.status === "off") return;
    this.error = clip(reason); this.status = "failed"; this.panel = null; this.queue = [];
    clearTimeout(this.deadline); this.worker?.terminate();
    if (!this.committed) this.reject?.(new Error(this.error));
    else { this.manager.revision++; this.manager.addOutput([{kind: "stopped", name: this.manifest.name, reason: this.error}]); }
  }
  stop() { clearTimeout(this.deadline); this.worker?.terminate(); this.status = "off"; this.panel = null; this.queue = []; }
}

class ModManager {
  constructor() {
    this.instances = new Map(); this.snapshot = {player: null, clock: null, motion: null}; this.output = []; this.nudge = null;
    this.revision = 1; this.indexRevision = null; this.closed = false; this.refreshing = false; this.refreshAgain = false;
    this.socket = null; this.reconnect = null; this.pollTimer = null;
    this.refreshPromise = null;
    this.failedUpdates = new Map();
    this.release = globalThis.horizonOptions?.release ?? null; this.updating = null;
    /* Content mods this page started with: id -> package hash. Blocks and items load once, before
       the game; a change reaches this page by reloading it (the gateway's release names them). */
    this.content = new Map(); this.contentPackages = [];
  }
  setContent(packages) {
    this.content = new Map(packages.map(row => [row.id, row.sha256])); this.contentPackages = packages;
  }
  /* For Rust's bootstrap, once (web/bootstrap.js takeHostedContent). */
  takeContent() { const packages = this.contentPackages; this.contentPackages = []; return packages; }
  /* The gateway names its client release; a different one means this page runs old code. */
  sawRelease(release) {
    if (typeof release !== "string" || !/^[a-f0-9]{16}$/.test(release) || !this.release || release === this.release
        || this.updating || this.closed) return;
    this.updating = release;
    let tried = null;
    try { tried = sessionStorage.getItem(RELOADED_KEY); } catch (_) {}
    if (tried === release) {
      // This page already reloaded for that release and is still older: never loop.
      this.addOutput([{kind: "feedback", text: "(dev)Modcraft was updated: reload the page to get the new version", error: true}]);
      return;
    }
    this.addOutput([{kind: "feedback", text: "(dev)Modcraft was updated: reloading in 10 seconds, you will rejoin right away", error: false}]);
    const started = performance.now();
    const attempt = () => {
      if (this.closed) return;
      // Not mid-jump or mid-fall: the world is the server's, but a reload in the air looks like a glitch.
      const motion = this.snapshot.motion;
      const settled = !motion || motion.on_ground === true || motion.steerable === false;
      if (document.hidden || settled || performance.now() - started > RELOAD_NOTICE_MS + RELOAD_SETTLE_MS) {
        try { sessionStorage.setItem(RELOADED_KEY, release); sessionStorage.setItem(RESUME_KEY, String(Date.now())); } catch (_) {}
        globalThis.horizonReloading = true;
        location.reload();
      } else setTimeout(attempt, 250);
    };
    setTimeout(attempt, document.hidden ? 0 : RELOAD_NOTICE_MS);
  }
  async checkRelease() {
    if (this.closed || this.updating) return;
    try { this.sawRelease(JSON.parse(new TextDecoder().decode(await boundedFetch("/api/release", 4096))).release); } catch (_) {}
  }
  addOutput(rows) {
    this.output.push(...rows);
    if (this.output.length > MAX_OUTPUT) this.output.splice(0, this.output.length - MAX_OUTPUT);
  }
  /* Movement nudges since the last poll: the latest vertical speed wins, pushes add (bounded in Rust again). */
  addNudge(nudge) {
    if (!nudge || typeof nudge !== "object") return;
    const vertical = Number.isFinite(nudge.vertical) ? Math.max(-3.92, Math.min(0.6, nudge.vertical)) : null;
    const push = Array.isArray(nudge.push) && nudge.push.length === 2 && nudge.push.every(Number.isFinite) ? nudge.push : [0, 0];
    const merged = this.nudge ?? {vertical: null, push: [0, 0]};
    if (vertical !== null) merged.vertical = vertical;
    let x = merged.push[0] + push[0], z = merged.push[1] + push[1]; const length = Math.hypot(x, z);
    if (length > 0.1) { x *= 0.1 / length; z *= 0.1 / length; }
    merged.push = [x, z]; this.nudge = merged;
  }
  refresh() {
    if (this.closed) return Promise.resolve();
    this.refreshAgain = true;
    if (this.refreshPromise) return this.refreshPromise;
    this.refreshPromise = (async () => {
      while (this.refreshAgain && !this.closed) { this.refreshAgain = false; await this.refreshOnce(); }
    })().finally(() => { this.refreshPromise = null; });
    return this.refreshPromise;
  }
  async refreshOnce() {
    this.refreshing = true;
    try {
      const index = await fetchIndex();
      // The game parses `manifest` strictly: keep it to horizon-mod.toml's fields. A capability this
      // page predates (a newer release went live) marks just that mod, which waits for a reload.
      const entries = index.mods.map(entry => {
        const normalized = manifest(entry);
        return {entry, manifest: normalized, unsupported: normalized.capabilities.some(c => !allowedCapabilities.has(c))};
      });
      if (new Set(entries.map(row => row.manifest.id)).size !== entries.length) throw new Error("Duplicate approved mod identifier");
      // Workers run client code: content-only and server mods have none here.
      if (entries.filter(row => row.manifest.side !== "server" && row.entry.entry).length > MAX_BROWSER_MODS) throw new Error("This world exceeds the limit of 8 active browser mods; ask the host to remove an older mod");
      const retained = new Set();
      for (const row of entries) {
        const id = row.manifest.id; retained.add(id);
        const old = this.instances.get(id);
        if (old?.hash === row.entry.sha256 && ["running", "failed", "off"].includes(old.status)) continue;
        if (this.failedUpdates.get(id) === row.entry.sha256) continue;
        if (row.unsupported) {
          this.checkRelease();
          if (old) old.stop();
          this.instances.set(id, placeholder(row, "off", "Needs a newer game: reload the page"));
          this.revision++;
          if (old?.hash !== row.entry.sha256) this.addOutput([{kind: "feedback", text: `${row.manifest.name} needs a newer game: reload the page to use it`, error: true}]);
          continue;
        }
        const content = isContent(row.manifest);
        if (content && this.content.get(id) !== row.entry.sha256) {
          // New or changed blocks and items: only a page that starts with them has them.
          this.checkRelease();
          if (old) old.stop();
          this.instances.set(id, placeholder(row, "off", "Reload the page to get its blocks and items"));
          this.revision++; continue;
        }
        if (row.manifest.side === "server" || !row.entry.entry) {
          // Its content (loaded at start) is live here; its code, if any, runs on the server.
          if (old) old.stop();
          this.instances.set(id, content ? placeholder(row, "running") : placeholder(row, "off", "Runs on the world's server"));
          this.revision++; continue;
        }
        const candidate = new Instance(this, row.entry, row.manifest);
        try {
          await candidate.initialize();
          if (this.closed) { candidate.stop(); return; }
          // Commit the new worker and retained UI first; retire the old instance
          // only after successful init. Each completed call is generation-bound.
          this.instances.set(id, candidate); candidate.commit(); old?.stop(); this.revision++;
          this.failedUpdates.delete(id);
        } catch (error) {
          candidate.stop();
          this.failedUpdates.set(id, row.entry.sha256);
          if (!old || old.status !== "running") { candidate.status = "failed"; candidate.error = errorText(error); this.instances.set(id, candidate); this.revision++; }
          this.addOutput([{kind: "feedback", text: `${row.manifest.name} update failed: ${errorText(error)}`, error: true}]);
        }
      }
      for (const [id, old] of this.instances) if (!retained.has(id)) { old.stop(); this.instances.delete(id); this.failedUpdates.delete(id); this.revision++; }
      // A removed content mod's blocks stay in this page until it reloads.
      for (const id of this.content.keys()) if (!retained.has(id)) this.checkRelease();
      this.indexRevision = index.revision;
    } catch (error) { this.addOutput([{kind: "feedback", text: "Approved mods unavailable: " + errorText(error), error: true}]); }
    finally {
      this.refreshing = false;
    }
  }
  connect() {
    if (this.closed) return;
    const url = new URL("/mods/events", location.href); url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    this.socket = new WebSocket(url);
    this.socket.onopen = () => this.refresh();
    this.socket.onmessage = event => {
      if (typeof event.data !== "string" || event.data.length > 4096) return;
      try { this.sawRelease(JSON.parse(event.data).release); } catch (_) {}
      this.refresh();
    };
    this.socket.onclose = () => { if (!this.closed) this.reconnect = setTimeout(() => this.connect(), 3000 + Math.random() * 2000); };
    this.socket.onerror = () => this.socket?.close();
  }
  view(drain = false) {
    const entries = [], panels = [], commands = new Set();
    for (const instance of this.instances.values()) {
      entries.push({manifest: instance.manifest, status: instance.status, error: instance.error});
      if (instance.status !== "running") continue;
      // Hidden until its startup commands have had their say.
      if (instance.panel && !instance.startup.length && !instance.queue.some(item => item.quiet) && !instance.busy?.quiet) panels.push(instance.panel);
      for (const command of instance.hooks.commands) if (commands.size < 128) commands.add(command);
    }
    const nudge = drain ? this.nudge : null;
    if (drain) this.nudge = null;
    return {revision: this.revision, entries, panels, commands: [...commands], out: drain ? this.output.splice(0, MAX_OUTPUT) : [], nudge};
  }
  dispatch(method, payload) {
    if (method === "view" || method === "poll") return this.view(method === "poll");
    if (method === "snapshot") {
      this.snapshot = payload && typeof payload === "object" ? {player: payload.player ?? null, clock: payload.clock ?? null, motion: payload.motion ?? null} : {player: null, clock: null, motion: null};
      return {};
    }
    if (method === "command") {
      const line = clip(payload, 4160).replace(/^\//, "").trim();
      const name = line.split(/\s/, 1)[0]; const args = line.slice(name.length).trimStart();
      for (const instance of this.instances.values()) if (instance.status === "running" && instance.hooks.commands.includes(name)) return {handled: instance.enqueue("command", {name, args})};
      return {handled: false};
    }
    if (["tick", "frame", "chat"].includes(method)) {
      // Content-only mods are running placeholders with no code to call.
      for (const instance of this.instances.values()) if (instance.status === "running") { instance.runStartup?.(); instance.enqueue(method, payload); }
    }
    return {};
  }
  close() {
    this.closed = true; clearTimeout(this.reconnect); clearInterval(this.pollTimer); this.socket?.close();
    for (const instance of this.instances.values()) instance.stop();
  }
}
export async function startMods() {
  if (globalThis.horizonMods) return globalThis.horizonMods;
  const manager = new ModManager();
  // The world's blocks and items first: without them this page must not join.
  try { manager.setContent(await contentPackages(await fetchIndex())); }
  catch (error) { throw new Error("The world's blocks and items could not be loaded: " + errorText(error) + ". Reload to try again."); }
  globalThis.horizonMods = manager;
  await manager.refresh(); manager.connect();
  manager.pollTimer = setInterval(() => { manager.refresh(); manager.checkRelease(); }, 30000);
  addEventListener("pagehide", () => manager.close(), {once: true});
  return manager;
}
