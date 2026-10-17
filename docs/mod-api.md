# Submit mods from a coding agent

The hosted submission API accepts a compiled `.hmod`, its source code, and review
notes. Authors do not need to open the Mod workshop. The host still approves the
exact package before it reaches players.

There are two mod rules:

1. **Don't break the game.** No crashes, world corruption, severe lag, or broken core gameplay.
2. **No free resources.** No granting or duplicating unearned items, materials, currency, or XP.

The machine-readable guide, including these rules, is available without a key at
`GET /api/v1/mods/guide`. Use the host's HTTPS origin. It describes the request
shape, limits, status URLs, and error responses.

## Agent workflow

The host gives each agent a private credential JSON with `origin` and `token`.
Keep it outside the repository with permissions `0600`. It grants only
`mods:submit` and `mods:read-own`. It cannot publish, approve, reject, play, or
read another key's submissions. Keys expire after 30 days by default and can be
revoked at any time. Don't put credentials in prompts, source bundles, URLs,
browser storage, or Git.

Build locally using the [mod SDK](mods.md). The server does not compile uploaded
Rust, run Cargo, execute build scripts, or run uploaded source. The executable
package goes through the existing native validator and sandbox.

```sh
cargo horizon-mod build
python3 /path/to/horizon/tools/modcraft_api.py \
  --config "$HOME/.config/modcraft/agent.json" submit \
  --project . --package target/horizon-mod/my_mod.hmod \
  --notes 'Adds a read-only HUD. Tested startup, toggling, and long sessions. No resources granted.'

python3 /path/to/horizon/tools/modcraft_api.py \
  --config "$HOME/.config/modcraft/agent.json" list

python3 /path/to/horizon/tools/modcraft_api.py \
  --config "$HOME/.config/modcraft/agent.json" status PACKAGE_SHA256
```

The client includes `Cargo.toml`, `Cargo.lock`, `horizon-mod.toml`, `build.rs`,
`src/**/*.rs`, and `wit/**/*.wit` when present. It rejects symlinks, traversal,
and hidden paths. Include additional text build inputs with repeatable
`--source relative/path`. SDK/dependency code is not copied automatically.
Review notes should explain the change and testing. The JSON response includes
the package hash, source hash, validation result, and `statusUrl`.

For a direct HTTP client:

```text
POST /api/v1/mods/submissions
Authorization: Bearer <agent key>
Content-Type: application/json

{
  "packageBase64": "<canonical base64 of .hmod bytes>",
  "notes": "What changed, how it was tested, and why it follows the two rules.",
  "files": [{"path": "src/lib.rs", "content": "<source text>"}]
}
```

`GET /api/v1/mods/submissions` lists that key's submissions; append the package
SHA-256 for one result. CLI requests do not need browser cookies, an `Origin`
header, or CSRF tokens. Cross-origin browser requests are rejected.

`202` means the package passed technical checks and awaits review. Identical
retries return the existing submission (`200`, or `422` if rejected), without
re-running validation. Source and notes are immutable for a package hash. For
changes, build a new version; the same package cannot be claimed by another key.

Statuses are `pending`, `rejected`, `published`, `superseded`, and `revoked`.
`published` means the exact package is in the active index. `422` includes a
validation error; `400` means malformed input, `401` means an invalid/expired/
revoked key, `409` means a duplicate conflict or full storage, and `413` means an
oversized request. On `429`, respect `Retry-After` before trying again.

Technical limits: 16 MiB package, 512 KiB of source across at most 64 files,
2,000 characters of notes, 24 MiB JSON request, six submissions per minute per
key, one validation at a time, 32 pending candidates, and 256 archived
submissions by default. Existing manifest, import/capability, content, size,
sandbox-init, and browser memory checks still apply. These checks don't prove
gameplay balance or complete safety: the host reviews the two rules. Attached
source is bound to the package hash but is not a reproducible-build attestation.

## Host access and review

Create an agent key without using the workshop. `--owner-key-file` contains your
host review key, **not** a cloud token or Discord secret. This file must also be
private. Give the agent only the resulting `agent.json`.

```sh
python3 tools/modcraft_api.py key-create \
  --origin https://YOUR-GAME-HOST \
  --owner-key-file /private/host-review-key \
  --label 'Alex coding agent' --days 30 \
  --out /private/alex-agent.json

python3 tools/modcraft_api.py key-list \
  --origin https://YOUR-GAME-HOST --owner-key-file /private/host-review-key

python3 tools/modcraft_api.py key-revoke KEY_ID \
  --origin https://YOUR-GAME-HOST --owner-key-file /private/host-review-key
```

The client writes a new mode-0600 credential file, never overwrites one, and
does not print the secret. The gateway persists only token digests in its
private `mod-api-tokens.json`. Restarting the gateway preserves issued keys;
revocation takes effect immediately. Token metadata endpoints require the
owner's session; creating/revoking also requires same-origin CSRF protection.

The workshop's **All submissions** view includes pending, live, rejected, older,
and removed versions, with a status filter. **Review source code** loads private
source on demand, rendered as text. Rejected/replaced packages retain review
source and metadata; unapproved executable files are never public.

Existing owner APIs remain available for review tools: `GET /api/mods/submissions`,
`GET /api/mods/submissions/HASH/source`, and `POST /api/mods/HASH/approve`,
`reject`, or `revoke` with `{ "sha256": "HASH" }`. These require a separate
owner session and CSRF token. Do not give submitting agents the host key.
Publication and notifications use the existing exact-hash approval path.

The source archive is bounded and never extracted or executed. When its quota
fills, submissions stop with `409` rather than deleting history. The host can
back up and archive old reviews during maintenance. No automatic approval or
world changes occur during submission.
