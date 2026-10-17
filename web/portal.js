const $ = id => document.getElementById(id);
let friend = {authenticated: false};
let owner = {authenticated: false};
let loading = false;
let submissions = [];
function status(id, message, error = false) { const node = $(id); node.textContent = message; node.classList.toggle("error", error); }
async function request(path, options = {}) {
  const response = await fetch(path, {credentials: "same-origin", cache: "no-store", redirect: "error", ...options});
  let data; try { data = await response.json(); } catch (_) { throw new Error("The server did not answer. Try again shortly."); }
  if (!response.ok) throw new Error(data.error ?? `Request failed (${response.status})`);
  return data;
}
function element(tag, text, className) { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (className) node.className = className; return node; }
function showSessions() {
  const ready = friend.authenticated && !friend.needsProfile;
  $("upload-form").hidden = !ready; $("friend-login").hidden = ready;
  status("friend-status", ready ? `Signed in as ${friend.name}.` : "Sign in with Discord and choose your player name to share a mod.");
  $("owner-form").hidden = owner.authenticated; $("owner-signed-in").hidden = !owner.authenticated;
  $("review-section").hidden = !owner.authenticated; $("active-section").hidden = !ready;
}
const capabilityLabels = {"game-read": "Read position and time", chat: "Write in local chat", hud: "Draw on your screen", registries: "Add world content"};
const checkLabels = {manifest: "Manifest", imports: "Permissions", content: "Content rules", archiveCaps: "File size limits", sandboxInitFuel: "Sandbox startup"};
function modRow(item, active = false) {
  const manifest = item.manifest ?? item;
  const row = element("li", undefined, "mod-item"); const top = element("div", undefined, "mod-top");
  const heading = element("h3", manifest.name ?? manifest.id ?? "Uploaded mod"); heading.append(element("span", manifest.version ?? "", "mod-version"));
  const labels = {pending: "Awaiting review", published: "Live", rejected: "Rejected", superseded: "Older version", revoked: "Removed"};
  top.append(heading, element("span", active ? "Live" : labels[item.status] ?? "Awaiting review", `badge ${active ? "approved" : item.status ?? "pending"}`)); row.append(top);
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
  if (item.created) row.append(element("p", `${item.submittedVia === "api" ? "Agent submission" : "Workshop upload"} · ${new Date(item.created).toLocaleString()}`));
  if (item.notes) row.append(element("p", item.notes, "submission-notes"));
  if (item.sourceSha256 && /^[a-f0-9]{64}$/.test(item.sha256)) {
    const source = element("details"); source.append(element("summary", `Review source code (${item.sourceFiles} ${item.sourceFiles === 1 ? "file" : "files"})`));
    const body = element("div"); source.append(body); let loaded = false, pending = false;
    source.addEventListener("toggle", async () => {
      if (!source.open || loaded || pending) return;
      pending = true; body.replaceChildren(element("p", "Loading source…"));
      try {
        const bundle = await request(`/api/mods/submissions/${item.sha256}/source`);
        body.replaceChildren(element("p", "The source is attached to this exact mod file for review. Its correspondence to the compiled mod has not been independently verified.", "owner-note"));
        for (const file of bundle.files) { const details = element("details"); details.append(element("summary", file.path), element("pre", file.content)); body.append(details); }
        loaded = true;
      } catch (error) { body.replaceChildren(element("p", error.message, "error")); }
      finally { pending = false; }
    });
    row.append(source);
  }
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
    const [history, active] = await Promise.all([owner.authenticated ? request("/api/mods/submissions") : null, friend.authenticated && !friend.needsProfile ? request("/mods/index.json") : null]);
    if (history) { submissions = [...history.submissions].reverse(); showSubmissions(); }
    if (active) { $("active-list").replaceChildren(...active.mods.map(row => modRow(row, true))); if (!active.mods.length) $("active-list").append(element("li", "No approved mods yet.", "empty")); }
  } catch (error) { status("review-status", error.message, true); }
  finally { loading = false; $("refresh").disabled = false; }
}
function showSubmissions() {
  const filter = $("submission-filter").value, rows = submissions.filter(row => filter === "all" || row.status === filter);
  $("pending-list").replaceChildren(...rows.map(row => modRow(row)));
  if (!rows.length) $("pending-list").append(element("li", "No submissions in this view yet.", "empty"));
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
  try { await request("/api/admin/logout", {method: "POST", headers: {"x-csrf-token": owner.csrf}}); owner = {authenticated: false}; submissions = []; $("pending-list").replaceChildren(); showSessions(); await refresh(); }
  catch (error) { status("owner-status", error.message, true); }
});
$("refresh").addEventListener("click", refresh);
$("submission-filter").addEventListener("change", showSubmissions);
request("/api/mods/rules").then(({rules}) => $("mod-rules").replaceChildren(...rules.map(rule => {
  const row = element("li"); row.append(element("strong", rule.text), element("span", ` ${rule.detail}`)); return row;
}))).catch(() => $("mod-rules").replaceChildren(element("li", "Don't break the game."), element("li", "No free resources.")));
try { [friend, owner] = await Promise.all([request("/api/session"), request("/api/admin/session")]); showSessions(); await refresh(); }
catch (error) { status("friend-status", "Could not reach the workshop."); status("owner-status", error.message, true); }
