const $ = id => document.getElementById(id);
let friend = {authenticated: false};
let owner = {authenticated: false};
let loading = false;
function status(id, message, error = false) { const node = $(id); node.textContent = message; node.classList.toggle("error", error); }
async function request(path, options = {}) {
  const response = await fetch(path, {credentials: "same-origin", cache: "no-store", redirect: "error", ...options});
  let data; try { data = await response.json(); } catch (_) { throw new Error("The server did not answer. Try again shortly."); }
  if (!response.ok) throw new Error(data.error ?? `Request failed (${response.status})`);
  return data;
}
function element(tag, text, className) { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }
function showSessions() {
  $("upload-form").hidden = !friend.authenticated; $("friend-login").hidden = friend.authenticated;
  status("friend-status", friend.authenticated ? `Signed in as ${friend.name}.` : "You'll need a player invite to share a mod.");
  $("owner-form").hidden = owner.authenticated; $("owner-signed-in").hidden = !owner.authenticated;
  $("review-section").hidden = !owner.authenticated; $("active-section").hidden = !friend.authenticated;
}
const capabilityLabels = {"game-read": "Read position and time", chat: "Write in local chat", hud: "Draw on your screen", registries: "Add world content"};
const checkLabels = {manifest: "Manifest", imports: "Permissions", content: "Content rules", archiveCaps: "File size limits", sandboxInitFuel: "Sandbox startup"};
function modRow(item, active = false) {
  const manifest = item.manifest ?? item;
  const row = element("li", undefined, "mod-item"); const top = element("div", undefined, "mod-top");
  const heading = element("h3", manifest.name ?? manifest.id ?? "Uploaded mod"); heading.append(element("span", manifest.version ?? "", "mod-version"));
  top.append(heading, element("span", active ? "Live" : (item.status ?? "pending"), `badge ${active ? "approved" : item.status ?? "pending"}`)); row.append(top);
  if (manifest.description) row.append(element("p", manifest.description));
  const side = manifest.side ?? "client";
  row.append(element("p", `${side === "both" ? "Players and server" : side === "server" ? "Server" : "Players"}${item.uploader ? ` · Uploaded by ${item.uploader}` : ""}`));
  const caps = element("div", undefined, "capabilities");
  for (const capability of manifest.capabilities ?? []) caps.append(element("span", capabilityLabels[capability] ?? capability));
  if (!caps.children.length) caps.append(element("span", "No optional permissions")); row.append(caps);
  if (item.checks) {
    const checks = element("ul", undefined, "checks");
    for (const [key, label] of Object.entries(checkLabels)) checks.append(element("li", `${item.checks[key] === true ? "✓" : "—"} ${label}`, item.checks[key] === true ? "" : "unchecked"));
    if (item.checks.browserMemoryCap) checks.append(element("li", `✓ ${Math.round(item.checks.browserMemoryCap / 1048576)} MB memory limit`));
    row.append(checks);
  }
  if (item.error) row.append(element("p", item.error, "error"));
  if (item.liveReload === false) row.append(element("p", "This changes world content and needs a coordinated server restart. It cannot be approved as a live update.", "owner-note"));
  row.append(element("div", "File fingerprint (SHA-256)", "hash-label"), element("code", item.sha256, "sha"));
  const details = element("details"); details.append(element("summary", "View manifest and checks"), element("pre", JSON.stringify({manifest, checks: item.checks ?? null}, null, 2))); row.append(details);
  if (owner.authenticated && /^[a-f0-9]{64}$/.test(item.sha256)) {
    const actions = element("div", undefined, "mod-actions");
    const action = (label, verb, css) => { const button = element("button", label, css); button.type = "button"; button.addEventListener("click", () => decide(item, verb, row)); actions.append(button); };
    if (active) action("Remove from world", "revoke", "secondary");
    else if (item.status === "pending") {
      if (item.liveReload !== false) action("Approve for everyone", "approve", "primary");
      action("Reject upload", "reject", "secondary");
    }
    row.append(actions);
  }
  return row;
}
async function decide(item, decision, row) {
  for (const button of row.querySelectorAll("button")) button.disabled = true;
  status("review-status", decision === "approve" ? "Publishing this exact version…" : "Saving your decision…");
  try {
    await request(`/api/mods/${item.sha256}/${decision}`, {method: "POST", headers: {"Content-Type": "application/json", "x-csrf-token": owner.csrf}, body: JSON.stringify({sha256: item.sha256})});
    status("review-status", decision === "approve" ? "Approved. Connected players are receiving the update." : decision === "revoke" ? "Removed from the world." : "Upload rejected.");
    await refresh();
  } catch (error) { status("review-status", error.message, true); }
  finally { for (const button of row.querySelectorAll("button")) button.disabled = false; }
}
async function refresh() {
  if (loading) return; loading = true; $("refresh").disabled = true;
  try {
    const [pending, active] = await Promise.all([owner.authenticated ? request("/api/mods/pending") : null, friend.authenticated ? request("/mods/index.json") : null]);
    if (pending) { const rows = [...pending.candidates].reverse(); $("pending-list").replaceChildren(...rows.map(row => modRow(row))); if (!rows.length) $("pending-list").append(element("li", "No uploads yet. Share a .hmod file to start a review.", "empty")); }
    if (active) { $("active-list").replaceChildren(...active.mods.map(row => modRow(row, true))); if (!active.mods.length) $("active-list").append(element("li", "No approved mods yet.", "empty")); }
  } catch (error) { status("review-status", error.message, true); }
  finally { loading = false; $("refresh").disabled = false; }
}
$("upload-form").addEventListener("submit", async event => {
  event.preventDefault(); const file = $("mod-file").files[0];
  if (!file || !file.name.toLowerCase().endsWith(".hmod") || file.size > 16 * 1024 * 1024 || !file.size) { status("upload-status", "Choose a .hmod file between 1 byte and 16 MB.", true); return; }
  const button = event.currentTarget.querySelector("button"); button.disabled = true; status("upload-status", "Uploading and checking your mod…");
  try {
    const result = await request("/api/mods/upload", {method: "POST", headers: {"Content-Type": "application/octet-stream", "x-csrf-token": friend.csrf}, body: file});
    status("upload-status", `${result.status === "published" ? "This exact file has already been approved." : "Checks passed. Waiting for the host's approval."} File: ${result.sha256}`); $("mod-file").value = ""; await refresh();
  } catch (error) { status("upload-status", error.message, true); }
  finally { button.disabled = false; }
});
$("owner-form").addEventListener("submit", async event => {
  event.preventDefault(); const secret = $("owner-secret").value; $("owner-secret").value = "";
  const button = event.currentTarget.querySelector("button"); button.disabled = true;
  try { owner = await request("/api/admin/login", {method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify({secret})}); status("owner-status", ""); showSessions(); await refresh(); }
  catch (error) { status("owner-status", error.message, true); }
  finally { button.disabled = false; }
});
$("owner-logout").addEventListener("click", async () => {
  try { await request("/api/admin/logout", {method: "POST", headers: {"x-csrf-token": owner.csrf}}); owner = {authenticated: false}; $("pending-list").replaceChildren(); showSessions(); await refresh(); }
  catch (error) { status("owner-status", error.message, true); }
});
$("refresh").addEventListener("click", refresh);
try { [friend, owner] = await Promise.all([request("/api/session"), request("/api/admin/session")]); showSessions(); await refresh(); }
catch (error) { status("friend-status", "Could not reach the workshop."); status("owner-status", error.message, true); }
