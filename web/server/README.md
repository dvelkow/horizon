# Authenticated game gateway and readonly mod catalog

Node 22 and the pinned `ws` dependency serve an explicit public build directory and a separate prepared-assets directory. `npm ci && npm test` checks friend sessions, asset access, fixed-upstream transport, identity checks, deadlines, WebSocket origins, and private-path exclusion.

Start with `node cli.mjs /private/path/gateway.json`. The configuration must be mode 0600 and outside every served directory. Required fields: `origin` (HTTPS), `inviteHash` (generate using `node hash-secret.mjs`), `privateDir`, `publicDir`, `assetsDir`, and `gamePort`. The gateway binds to loopback behind TLS; `listenHost` defaults to `127.0.0.1` and `listenPort` to 8080. Loopback HTTP requires explicit `allowInsecureLocalhost: true` for development. Production origin, secrets and machine addresses are operator configuration, never source or browser bundles.

`POST /api/login` takes JSON `{invite, name}` and issues a Secure, HttpOnly, SameSite cookie; `GET /api/session` restores the session. `POST /api/logout` requires the session CSRF token in `x-csrf-token`. The binary `/game` WebSocket always connects to fixed `127.0.0.1:gamePort`, checks the Minecraft login identity against the session, and limits payloads, queues and deadlines. No browser can choose an upstream address. Reserved player names cannot be used in this phase.

Run the native dedicated engine on loopback using `horizon-server --dir /private/world run` and the supplied server configuration. Keep game, configuration and cloud provisioning credentials outside the static roots. COOP/COEP and CSP headers are supplied centrally. See [small-host memory results](deploy/memory-results.md) for the measured effect of the dedicated-resource reductions.

Phase 3 adds a verified, operator-installed readonly mod catalog and `/mods-runtime/` Workers; see [browser components](../../docs/browser-mods.md). Upload and owner approval routes are not implemented yet.
