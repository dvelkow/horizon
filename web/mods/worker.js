/* One approved WebAssembly component per Worker. Only explicit WIT imports reach it. */
let events;
let manifest;
let hooks;
let snapshot = {player: null, clock: null, motion: null};
let panel = null;
let output = [];
let nudge = null;
let chatCount = 0;
let logCount = 0;
let initialized = false;
const corners = new Set(["top-left", "top-right", "bottom-left", "bottom-right"]);
const clip = (value, maximum = 256) => Array.from(String(value).slice(0, maximum * 2)).slice(0, maximum).join("");
const finite = value => Number.isFinite(Number(value)) ? Number(value) : 0;
/* The bounds of crates/horizon-mods/src/nudge.rs; the game bounds nudges again. */
const MAX_RISE = 0.6, MAX_DROP = 3.92, MAX_PUSH = 0.1;
function beginCall(nextSnapshot) {
  if (nextSnapshot) snapshot = nextSnapshot;
  output = []; chatCount = 0; logCount = 0; nudge = null;
}
function player() {
  const value = snapshot.player;
  if (!value) return undefined;
  return {x: finite(value.x), y: finite(value.y), z: finite(value.z), yaw: finite(value.yaw), pitch: finite(value.pitch),
    health: finite(value.health), food: Math.max(0, Math.min(0xffffffff, finite(value.food))) >>> 0,
    dimension: clip(value.dimension, 128)};
}
function motion() {
  const value = snapshot.motion;
  if (!value) return undefined;
  return {vx: finite(value.vx), vy: finite(value.vy), vz: finite(value.vz), onGround: value.on_ground === true,
    jumping: value.jumping === true, steerable: value.steerable === true,
    liftLeft: Math.max(0, Math.min(0xffffffff, finite(value.lift_left))) >>> 0};
}
function pushNudge(dx, dz) {
  if (!Number.isFinite(dx) || !Number.isFinite(dz)) return;
  nudge ??= {vertical: null, push: [0, 0]};
  let x = nudge.push[0] + dx, z = nudge.push[1] + dz; const length = Math.hypot(x, z);
  if (length > MAX_PUSH) { x *= MAX_PUSH / length; z *= MAX_PUSH / length; }
  nudge.push = [x, z];
}
function clock() {
  if (!snapshot.clock) return undefined;
  const number = value => { try { return BigInt.asUintN(64, BigInt(value)); } catch (_) { return 0n; } };
  return {gameTime: number(snapshot.clock.game_time), dayTime: number(snapshot.clock.day_time)};
}
function importsFor(capabilities) {
  const imports = Object.create(null);
  const add = (name, functions) => {
    const instance = Object.freeze(functions);
    // Mods built for 0.1.0 and 0.1.1 link the same functions.
    imports[`horizon:mods/${name}@0.1.0`] = instance;
    imports[`horizon:mods/${name}@0.1.1`] = instance;
    imports[`horizon:mods/${name}`] = instance;
  };
  add("log", {write(level, message) {
    if (++logCount <= 16) output.push({kind: "log", level: ["debug", "info", "warn", "error"].includes(level) ? level : "info", text: clip(message)});
  }});
  if (capabilities.includes("game-read")) add("game", {getPlayer: player, getClock: clock});
  if (capabilities.includes("chat")) add("chat", {show(text) {
    if (++chatCount <= 8) output.push({kind: "chat", name: manifest.name, text: clip(text)});
  }});
  if (capabilities.includes("hud")) add("hud", {
    setPanel(corner, lines) {
      if (!corners.has(corner) || !Array.isArray(lines)) throw new Error("Invalid HUD output");
      panel = {corner, lines: lines.slice(0, 16).map(line => ({text: clip(line.text), color: Number(line.color) >>> 0}))};
    },
    clear() { panel = null; },
  });
  if (capabilities.includes("movement")) add("movement", {
    getMotion: motion,
    setVerticalSpeed(vy) {
      if (!Number.isFinite(vy)) return;
      nudge ??= {vertical: null, push: [0, 0]};
      nudge.vertical = Math.max(-MAX_DROP, Math.min(MAX_RISE, vy));
    },
    push: pushNudge,
  });
  return Object.freeze(imports);
}
function result(value) {
  if (value && value.tag === "err") throw new Error(clip(value.val));
  return value && value.tag === "ok" ? value.val : value;
}
function registration(value) {
  value = result(value);
  if (!value || !Array.isArray(value.commands)) throw new Error("Mod returned invalid registration");
  return {commands: [...new Set(value.commands.filter(c => typeof c === "string" && /^[a-z][a-z0-9_:-]{0,63}$/.test(c)))].slice(0, 128),
    tick: value.tick === true, frame: value.frame === true, chat: value.chat === true};
}
async function initialize(message) {
  if (initialized) throw new Error("Mod Worker has already initialized");
  initialized = true; manifest = message.manifest;
  const files = new Map(message.files.map(file => [file.path, file.bytes]));
  const code = files.get(message.entry);
  if (!(code instanceof ArrayBuffer)) throw new Error("Missing verified mod entry");
  const moduleUrl = URL.createObjectURL(new Blob([code], {type: "text/javascript"}));
  try {
    const generated = await import(moduleUrl);
    if (typeof generated.instantiate !== "function") throw new Error("Mod glue has no component instantiator");
    const getCoreModule = async path => {
      const name = String(path).replace(/^\.\//, "");
      if (!/^[a-zA-Z0-9_.\/-]+\.wasm$/.test(name) || name.split("/").includes("..")) throw new Error("Invalid core module path");
      const bytes = files.get(name);
      if (!(bytes instanceof ArrayBuffer)) throw new Error("Missing verified core module");
      return WebAssembly.compile(bytes);
    };
    beginCall(message.snapshot);
    const component = await generated.instantiate(getCoreModule, importsFor(manifest.capabilities));
    events = component["horizon:mods/events@0.1.1"] || component["horizon:mods/events@0.1.0"] || component["horizon:mods/events"] || component.events;
    if (!events || typeof events.init !== "function") throw new Error("Component does not export Horizon events");
    hooks = registration(events.init());
    // There is no movement step to nudge before the first tick.
    nudge = null;
    postMessage({kind: "ready", hooks, panel, out: output});
  } finally { URL.revokeObjectURL(moduleUrl); }
}
function invoke(message) {
  if (!events) throw new Error("Mod is not initialized");
  beginCall(message.snapshot);
  if (message.method === "tick" && hooks.tick) events.tick(BigInt.asUintN(64, BigInt(message.payload)));
  else if (message.method === "frame" && hooks.frame) events.frame(finite(message.payload));
  else if (message.method === "chat" && hooks.chat) events.chat(clip(message.payload, 4096));
  else if (message.method === "command") {
    try {
      const answer = result(events.command(message.payload.name, clip(message.payload.args, 4096)));
      if (answer !== undefined && answer !== null) output.push({kind: "feedback", text: clip(answer), error: false});
    } catch (error) {
      // ComponentError is the WIT result's normal error branch. Wasm traps must
      // stop this mod, rather than masquerading as ordinary command feedback.
      if (error instanceof WebAssembly.RuntimeError) throw error;
      if (error?.name !== "ComponentError" && !(error && Object.hasOwn(error, "payload"))) throw error;
      output.push({kind: "feedback", text: clip(error.payload ?? error.message ?? error), error: true});
    }
  } else throw new Error("Unregistered mod hook");
  postMessage({kind: "result", call: message.call, panel, out: output, nudge});
}
self.onmessage = async event => {
  try {
    const message = event.data;
    if (message.kind === "initialize") await initialize(message);
    else if (message.kind === "call") invoke(message);
    else throw new Error("Unknown mod message");
  } catch (error) {
    postMessage({kind: "failed", reason: clip(error?.payload ?? error?.message ?? error)});
    close();
  }
};
