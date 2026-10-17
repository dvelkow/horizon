import { createServer } from 'node:http';
import * as fs from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { pipeline } from 'node:stream/promises';
import { resolve, sep, extname } from 'node:path';
import { isIP } from 'node:net';
import { WebSocketServer } from 'ws';
import { Sessions, RateLimit, validSecretHash, verifySecret, cookieValue, sessionCookie, equalToken } from './security.mjs';
import { ModStore } from './mod-store.mjs';
import { bridgeGame } from './bridge.mjs';
import { Accounts, ProfileError, SKINS, SESSION_SECONDS } from './accounts.mjs';
import { DiscordAuth, oauthCookie } from './discord.mjs';
import { MinecraftSkins, COPIED_SKIN } from './minecraft-skins.mjs';
import { ModApiTokens, ModApiError, MOD_RULES, apiGuide, parseSubmission, submissionView } from './mod-api.mjs';

class HttpError extends Error { constructor(status, message) { super(message); this.status = status; } }
const fail = (status, message) => { throw new HttpError(status, message); };
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm', '.json': 'application/json; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.ogg': 'audio/ogg', '.hmod': 'application/octet-stream' };

async function body(request, limit) {
  const declared = request.headers['content-length'];
  if (declared && (!/^\d+$/.test(declared) || Number(declared) > limit)) fail(413, 'Request exceeds its size limit');
  const chunks = []; let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > limit) fail(413, 'Request exceeds its size limit');
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, size);
}
async function jsonBody(request, limit = 8192) {
  if (request.headers['content-type']?.split(';')[0] !== 'application/json') fail(415, 'JSON content type required');
  try {
    const value = JSON.parse((await body(request, limit)).toString('utf8'));
    if (!value || typeof value !== 'object' || Array.isArray(value)) fail(400, 'A JSON object is required');
    return value;
  }
  catch (error) { if (error instanceof HttpError) throw error; fail(400, 'Invalid JSON body'); }
}
function json(response, status, value, headers = {}) {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store', ...headers });
  response.end(`${JSON.stringify(value)}\n`);
}
function securityHeaders(response, secure) {
  response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  response.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
  response.setHeader('X-Content-Type-Options', 'nosniff');
  response.setHeader('Referrer-Policy', 'no-referrer');
  response.setHeader('X-Frame-Options', 'DENY');
  response.setHeader('Permissions-Policy', 'camera=(), microphone=(), geolocation=(), payment=(), usb=()');
  response.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' blob:; worker-src 'self' blob:; connect-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'");
  if (secure) response.setHeader('Strict-Transport-Security', 'max-age=31536000');
}

async function staticFile(request, response, root, relative, { immutable = false } = {}) {
  if (!root || relative.length > 512 || relative.split('/').some(part => !part || part === '.' || part === '..' || part.startsWith('.')) || relative.includes('\\')) fail(404, 'Not found');
  let realRoot, file;
  try { realRoot = await fs.realpath(root); file = await fs.realpath(resolve(realRoot, relative)); }
  catch { fail(404, 'Not found'); }
  if (!file.startsWith(`${realRoot}${sep}`)) fail(404, 'Not found');
  const stat = await fs.stat(file);
  if (!stat.isFile()) fail(404, 'Not found');
  response.writeHead(200, { 'Content-Type': MIME[extname(file)] ?? 'application/octet-stream', 'Content-Length': stat.size,
    'Cache-Control': immutable ? 'private, max-age=31536000, immutable' : 'private, no-cache' });
  if (request.method === 'HEAD') response.end();
  else await pipeline(createReadStream(file), response);
}

function validateConfig(input) {
  const config = { ...input };
  const origin = new URL(config.origin);
  const local = ['localhost', '127.0.0.1', '[::1]'].includes(origin.hostname);
  if (origin.origin !== config.origin || (origin.protocol !== 'https:' && !(config.allowInsecureLocalhost === true && local && origin.protocol === 'http:'))) throw new Error('Use an HTTPS origin, or explicitly allow loopback HTTP for development');
  if (!validSecretHash(config.adminHash) || (!config.discord && !validSecretHash(config.inviteHash)) || config.inviteHash === config.adminHash) throw new Error('An owner hash and Discord credentials (or legacy invite hash) are required');
  if (!Number.isInteger(config.gamePort) || config.gamePort < 1 || config.gamePort > 65535) throw new Error('A fixed gamePort is required');
  for (const field of ['privateDir', 'publicDir', 'assetsDir']) if (typeof config[field] !== 'string') throw new Error(`${field} is required`);
  const privateDir = resolve(config.privateDir);
  for (const directory of [config.publicDir, config.assetsDir, config.modsRuntimeDir].filter(Boolean).map(value => resolve(value))) {
    if (privateDir === directory || privateDir.startsWith(`${directory}${sep}`) || directory.startsWith(`${privateDir}${sep}`)) throw new Error('Private storage must be separate from every served directory');
  }
  config.originHost = origin.host; config.secure = origin.protocol === 'https:';
  config.protectedNames = new Set((config.protectedNames ?? ['admin', 'owner']).map(name => name.toLowerCase()));
  return config;
}

/** One process owns the candidate/index writes. Run one instance against this privateDir. */
export async function createGateway(input, { discordFetch, minecraftFetch } = {}) {
  // Resolve roots before comparing them: a symlinked "public" directory must
  // never turn private state into a static asset tree.
  await fs.mkdir(input.privateDir, { recursive: true, mode: 0o700 });
  const roots = { ...input };
  for (const key of ['privateDir', 'publicDir', 'assetsDir', 'modsRuntimeDir']) if (roots[key]) roots[key] = await fs.realpath(roots[key]);
  const config = validateConfig(roots);
  const store = new ModStore(config); await store.init();
  const apiTokens = new ModApiTokens(config.privateDir); await apiTokens.init();
  const minecraftSkins = new MinecraftSkins(config.privateDir, minecraftFetch); await minecraftSkins.init();
  const accounts = config.discord ? new Accounts(config.privateDir, { protectedNames: [...config.protectedNames], ownerIds: config.discord.ownerIds ?? [], skinStore: minecraftSkins }) : null;
  if (accounts) await accounts.init();
  const discord = config.discord ? new DiscordAuth(config.discord, config.origin, discordFetch) : null;
  const friends = accounts ?? new Sessions({ capacity: config.maxSessions ?? 128 });
  const admins = new Sessions({ capacity: 8, lifetimeMs: 20 * 60 * 1000 });
  const friendCookie = config.secure ? '__Host-horizon_session' : 'horizon_session';
  const adminCookie = config.secure ? '__Host-horizon_admin' : 'horizon_admin';
  const flowCookie = config.secure ? '__Host-modcraft_oauth' : 'modcraft_oauth';
  const logins = new RateLimit({ limit: config.loginAttempts ?? 10, windowMs: 60_000 });
  const writes = new RateLimit({ limit: 30, windowMs: 60_000 });
  const apiRequests = new RateLimit({ limit: 120, windowMs: 60_000 });
  const apiWrites = new RateLimit({ limit: 6, windowMs: 60_000 });
  const skinLookups = new RateLimit({ limit: 6, windowMs: 60_000 });
  const upgrades = new RateLimit({ limit: 30, windowMs: 60_000 });
  let passwordChecks = 0, validating = false;
  const sockets = new Map(), modSockets = new Set();
  const wss = new WebSocketServer({ noServer: true, maxPayload: 2 * 1024 * 1024, maxFragments: 256,
    maxBufferedChunks: 1024, perMessageDeflate: false, clientTracking: true });
  const peer = request => {
    const address = request.socket.remoteAddress ?? 'unknown';
    if (config.trustLoopbackProxy && ['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(address)) {
      const forwarded = request.headers['x-forwarded-for']?.split(',').at(-1)?.trim();
      if (forwarded && isIP(forwarded)) return forwarded;
    }
    return address;
  };
  const host = request => { if (request.headers.host !== config.originHost) fail(421, 'Unexpected host'); };
  const origin = request => { if (request.headers.origin !== config.origin) fail(403, 'Origin rejected'); };
  const signedIn = request => friends.get(cookieValue(request, friendCookie)) ?? fail(401, 'Sign in with Discord first');
  const friend = request => { const session = signedIn(request); if (!session.name) fail(403, 'Choose your in-game name and skin first'); return session; };
  const publicSession = session => session ? { authenticated: true, name: session.name, skin: session.skin, displayName: session.displayName,
    ...(session.customSkin ? {customSkin: minecraftSkins.latest(session.minecraftName,session.customSkin)} : {}),
    needsProfile: !session.name, csrf: session.csrf, expires: session.expires, authProvider: discord ? 'discord' : 'invite' }
    : { authenticated: false, discordAvailable: !!discord };
  const redirect = (response, location, cookies = []) => { response.writeHead(303, { Location: location, 'Cache-Control': 'no-store', ...(cookies.length ? { 'Set-Cookie': cookies } : {}) }); response.end(); };
  const admin = request => admins.get(cookieValue(request, adminCookie)) ?? fail(401, 'Owner sign-in required');
  const csrf = (request, session) => { origin(request); if (!equalToken(request.headers['x-csrf-token'], session.csrf)) fail(403, 'CSRF token rejected'); };
  const agent = request => {
    if (!apiRequests.take(peer(request))) fail(429, 'Too many API requests; retry shortly');
    // Bearer credentials never fall back to browser cookies or query strings.
    // No cross-origin browser API access; command-line agents omit Origin.
    if (request.headers.origin && request.headers.origin !== config.origin) fail(403, 'Origin rejected');
    return apiTokens.authenticate(request.headers.authorization) ?? fail(401, 'A valid submission API key is required');
  };
  const closeSession = id => { for (const [ws, value] of sockets) if (value === id) ws.close(1008, 'Signed out'); };
  const notify = () => {
    const message = JSON.stringify({ type: 'mods-changed', revision: store.index.revision });
    for (const ws of modSockets) {
      if (ws.bufferedAmount > 64 * 1024) ws.terminate();
      else if (ws.readyState === ws.OPEN) ws.send(message);
    }
  };

  const server = createServer(async (request, response) => {
    securityHeaders(response, config.secure);
    try {
      host(request);
      let path;
      try { path = decodeURIComponent(new URL(request.url, config.origin).pathname); } catch { fail(400, 'Invalid request path'); }
      if (request.method === 'GET' && path === '/healthz') return json(response, 200, { ok: true });
      if (request.method === 'GET' && path === '/api/v1/mods/guide') return json(response, 200, apiGuide(config.origin));
      if (request.method === 'GET' && path === '/api/mods/rules') return json(response, 200, { rules: MOD_RULES });
      if (path === '/api/admin/mod-api-tokens') {
        const session = admin(request);
        if (request.method === 'GET') return json(response, 200, { keys: apiTokens.list() });
        if (request.method === 'POST') {
          csrf(request, session);
          if (!writes.take(session.id)) fail(429, 'Too many key changes');
          return json(response, 201, await apiTokens.create(await jsonBody(request)));
        }
        fail(405, 'Method not allowed');
      }
      const revokeKey = /^\/api\/admin\/mod-api-tokens\/([a-f0-9]{16})\/revoke$/.exec(path);
      if (request.method === 'POST' && revokeKey) {
        const session = admin(request); csrf(request, session);
        return json(response, 200, await apiTokens.revoke(revokeKey[1]));
      }
      const submissionPath = /^\/api\/v1\/mods\/submissions(?:\/([a-f0-9]{64}))?$/.exec(path);
      if (submissionPath) {
        const key = agent(request), hash = submissionPath[1];
        const view = candidate => ({ ...submissionView(candidate, store.index), statusUrl: `${config.origin}/api/v1/mods/submissions/${candidate.sha256}` });
        if (request.method === 'GET') {
          if (!hash) return json(response, 200, { submissions: (await store.history()).filter(c => c.agentId === key.id).map(view) });
          let candidate;
          try { candidate = await store.readCandidate(hash); } catch (error) { if (error.code !== 'ENOENT') throw error; }
          if (!candidate || candidate.agentId !== key.id) fail(404, 'Submission not found');
          return json(response, 200, view(candidate));
        }
        if (request.method !== 'POST' || hash) fail(405, 'Method not allowed');
        if (validating || !apiWrites.take(key.id)) fail(429, 'Validation is busy or the submission rate limit was reached');
        validating = true;
        try {
          const review = parseSubmission(await jsonBody(request, 24 * 1024 * 1024), config.maxUploadBytes ?? 16 * 1024 * 1024);
          let previous;
          try { previous = await store.readCandidate(review.source.packageSha256); } catch (error) { if (error.code !== 'ENOENT') throw error; }
          if (previous) {
            if (previous.agentId !== key.id || previous.sourceSha256 !== review.sourceSha256) fail(409, 'This package already has different review material; build a new version');
            return json(response, previous.status === 'rejected' ? 422 : 200, view(previous));
          }
          const history = await store.history();
          if (history.length >= (config.maxSubmissionHistory ?? 256) || (await store.pending()).length >= (config.maxCandidates ?? 32)) fail(409, 'Submission storage is full; ask the host to review or archive submissions');
          const candidate = await store.upload(review.bytes, key.label, { ...review, agentId: key.id });
          return json(response, candidate.status === 'rejected' ? 422 : 202, view(candidate), { Location: view(candidate).statusUrl });
        } finally { validating = false; }
      }
      if (request.method === 'GET' && path === '/api/mods/submissions') {
        admin(request); return json(response, 200, { submissions: (await store.history()).map(c => submissionView(c, store.index)) });
      }
      const sourcePath = /^\/api\/mods\/submissions\/([a-f0-9]{64})\/source$/.exec(path);
      if (request.method === 'GET' && sourcePath) {
        admin(request);
        let source;
        try { source = await store.readSource(sourcePath[1]); } catch (error) { if (error.code !== 'ENOENT') throw error; }
        if (!source) fail(404, 'No source attached to this submission');
        return json(response, 200, source);
      }
      if (request.method === 'GET' && path === '/auth/discord') {
        if (!discord) fail(503, 'Discord sign-in is being connected. Please try again shortly');
        if (!logins.take(`${peer(request)}:discord`)) fail(429, 'Too many sign-in attempts; retry later');
        if (friends.get(cookieValue(request, friendCookie))) return redirect(response, '/');
        const flow = discord.begin();
        return redirect(response, flow.url, [oauthCookie(flowCookie, flow.binding, 600, config.secure)]);
      }
      if (request.method === 'GET' && path === '/auth/discord/callback') {
        if (!discord) fail(404, 'Not found');
        if (!logins.take(`${peer(request)}:discord-callback`)) fail(429, 'Too many sign-in attempts; retry later');
        const cookies = [oauthCookie(flowCookie, '', 0, config.secure)];
        try {
          const identity = await discord.finish(new URL(request.url, config.origin).searchParams, cookieValue(request, flowCookie));
          if (!identity) return redirect(response, '/?auth=cancelled', cookies);
          const previous = cookieValue(request, friendCookie);
          const session = await accounts.signIn(identity, previous);
          if (previous) closeSession(previous);
          cookies.push(sessionCookie(friendCookie, session.id, SESSION_SECONDS, config.secure));
          return redirect(response, '/', cookies);
        } catch { return redirect(response, '/?auth=failed', cookies); }
      }
      if (request.method === 'GET' && path === '/api/session') {
        const session = friends.get(cookieValue(request, friendCookie));
        return json(response, 200, publicSession(session));
      }
      if (request.method === 'GET' && path === '/api/skins') return json(response, 200, { skins: SKINS.map(id => ({ id, name: id[0].toUpperCase() + id.slice(1), url: `/skins/${id}.png` })) });
      if (request.method === 'POST' && path === '/api/skins/minecraft') {
        const session = signedIn(request); csrf(request, session);
        if (!accounts) fail(404, 'Not found');
        if (!skinLookups.take(session.discordId)) fail(429, 'Too many skin lookups. Try again in a minute');
        return json(response, 200, {skin: await minecraftSkins.copy((await jsonBody(request))?.name)});
      }
      if (request.method === 'POST' && path === '/api/profile') {
        if (!accounts) fail(404, 'Not found');
        const session = signedIn(request); csrf(request, session);
        if (!writes.take(session.id)) fail(429, 'Too many profile changes; retry shortly');
        const updated = await accounts.saveProfile(session, await jsonBody(request), !!admins.get(cookieValue(request, adminCookie)));
        return json(response, 200, publicSession(updated));
      }
      if (request.method === 'GET' && path === '/api/players/skins') {
        const session=friend(request), activeNames=new Set([session.name]);
        for(const id of sockets.values()) { const user=friends.get(id); if(user?.name) activeNames.add(user.name); }
        return json(response, 200, accounts?.skins(activeNames) ?? { revision: 0, players: {} });
      }
      if (request.method === 'GET' && path === '/api/admin/session') {
        const session = admins.get(cookieValue(request, adminCookie));
        return json(response, 200, session ? { authenticated: true, csrf: session.csrf, expires: session.expires } : { authenticated: false });
      }
      if (request.method === 'POST' && ['/api/login', '/api/admin/login'].includes(path)) {
        origin(request);
        if (path === '/api/login' && discord) fail(410, 'Invite sign-in has been replaced by Discord');
        if (!logins.take(`${peer(request)}:${path}`) || passwordChecks >= 2) fail(429, 'Too many sign-in attempts; retry later');
        const value = await jsonBody(request), owner = path === '/api/admin/login';
        if (!value || typeof value !== 'object' || Array.isArray(value)) fail(400, 'A sign-in object is required');
        passwordChecks++;
        let verified;
        try { verified = await verifySecret(owner ? value.secret : value.invite, owner ? config.adminHash : config.inviteHash); }
        finally { passwordChecks--; }
        if (!verified) fail(401, 'Sign-in rejected');
        if (!owner && (typeof value.name !== 'string' || !/^[A-Za-z0-9_]{1,16}$/.test(value.name))) fail(400, 'Use a player name with 1–16 letters, digits or underscores');
        if (!owner && config.protectedNames.has(value.name.toLowerCase()) && !admins.get(cookieValue(request, adminCookie))) fail(403, 'That player name is reserved for the owner');
        const collection = owner ? admins : friends, cookie = owner ? adminCookie : friendCookie;
        const previous = cookieValue(request, cookie); await collection.delete(previous); if (!owner && previous) closeSession(previous);
        const session = collection.create(owner ? {} : { name: value.name });
        return json(response, 200, { authenticated: true, name: session.name, csrf: session.csrf, expires: session.expires }, {
          'Set-Cookie': sessionCookie(cookie, session.id, owner ? 1200 : 28800, config.secure),
        });
      }
      if (request.method === 'POST' && ['/api/logout', '/api/admin/logout'].includes(path)) {
        const owner = path === '/api/admin/logout', session = owner ? admin(request) : signedIn(request); csrf(request, session);
        await (owner ? admins : friends).delete(session.id); if (!owner) closeSession(session.id);
        return json(response, 200, { ok: true }, { 'Set-Cookie': sessionCookie(owner ? adminCookie : friendCookie, '', 0, config.secure) });
      }
      if (request.method === 'POST' && path === '/api/mods/upload') {
        const session = friend(request); csrf(request, session);
        if (!writes.take(session.id) || validating) fail(429, 'A mod is already being checked; retry shortly');
        if (request.headers['content-type']?.split(';')[0] !== 'application/octet-stream') fail(415, 'Upload .hmod bytes as application/octet-stream');
        validating = true;
        try {
          const bytes = await body(request, config.maxUploadBytes ?? 16 * 1024 * 1024);
          if (!bytes.length) fail(400, 'The upload is empty');
          const candidate = await store.upload(bytes, session.name);
          return json(response, candidate.status === 'rejected' ? 422 : 202, { sha256: candidate.sha256, status: candidate.status, error: candidate.error });
        } finally { validating = false; }
      }
      if (request.method === 'GET' && path === '/api/mods/pending') { admin(request); return json(response, 200, { candidates: await store.pending() }); }
      const decision = /^\/api\/mods\/([a-f0-9]{64})\/(approve|reject|revoke)$/.exec(path);
      if (request.method === 'POST' && decision) {
        const session = admin(request); csrf(request, session);
        const value = await jsonBody(request);
        if (value.sha256 !== decision[1]) fail(409, 'Approval hash does not match the reviewed candidate');
        const previousRevision = store.index.revision;
        try { await store[decision[2]](decision[1]); }
        catch (error) { fail(error.code === 'ENOENT' ? 404 : 409, error.message); }
        finally { if (store.index.revision !== previousRevision) notify(); }
        return json(response, 200, { ok: true, revision: store.index.revision });
      }
      if (request.method === 'GET' && path === '/mods/index.json') { friend(request); return json(response, 200, { schema: 1, revision: store.index.revision, mods: store.index.mods }); }
      if (!['GET', 'HEAD'].includes(request.method)) fail(405, 'Method not allowed');
      if (path.startsWith('/api/') || path === '/game' || path === '/mods/events') fail(404, 'Not found');
      if (path.startsWith('/player-skins/')) {
        signedIn(request);
        const id=path.slice('/player-skins/'.length,-4);
        if (!path.endsWith('.png') || !COPIED_SKIN.test(id) || !minecraftSkins.describe(id)) fail(404,'Not found');
        return await staticFile(request,response,minecraftSkins.root,id+'.png',{immutable:true});
      }
      if (path.startsWith('/mods/')) {
        friend(request);
        const file = store.publishedPath(path);
        if (!file) fail(404, 'Not found');
        return await staticFile(request, response, config.privateDir, file.slice(resolve(config.privateDir).length + 1), { immutable: true });
      }
      if (path.startsWith('/assets/')) { friend(request); return await staticFile(request, response, config.assetsDir, path.slice('/assets/'.length)); }
      if (path.startsWith('/mods-runtime/')) { friend(request); return await staticFile(request, response, config.modsRuntimeDir, path.slice('/mods-runtime/'.length)); }
      return await staticFile(request, response, config.publicDir, path === '/' ? 'index.html' : path.slice(1));
    } catch (error) {
      if (response.headersSent || response.destroyed) { response.destroy(); return; }
      const status = error instanceof HttpError || error instanceof ProfileError || error instanceof ModApiError ? error.status : 500;
      json(response, status, { error: status === 500 ? 'Gateway request failed' : error.message }, { Connection: 'close', ...(status === 429 ? { 'Retry-After': '60' } : {}) });
    }
  });
  server.requestTimeout = 30_000; server.headersTimeout = 10_000; server.keepAliveTimeout = 5000; server.maxHeadersCount = 64;
  server.on('clientError', (_error, socket) => { if (socket.writable) socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n'); });
  server.on('upgrade', (request, socket, head) => {
    try {
      host(request); origin(request);
      const path = new URL(request.url, config.origin).pathname;
      if (!['/game', '/mods/events'].includes(path)) fail(404, 'Not found');
      if (new URL(request.url, config.origin).search) fail(400, 'WebSocket query parameters are not accepted');
      const session = friend(request);
      if (!upgrades.take(session.id) || wss.clients.size >= (config.maxConnections ?? 32)) fail(429, 'Connection limit reached');
      if ([...sockets.values()].filter(id => id === session.id).length >= 4) fail(429, 'Too many connections for this session');
      wss.handleUpgrade(request, socket, head, ws => {
        sockets.set(ws, session.id); ws.alive = true;
        ws.on('pong', () => { ws.alive = true; });
        ws.on('close', () => { sockets.delete(ws); modSockets.delete(ws); });
        ws.on('error', () => ws.terminate());
        if (path === '/game') bridgeGame(ws, session, config);
        else {
          modSockets.add(ws);
          ws.send(JSON.stringify({ type: 'mods-changed', revision: store.index.revision }));
          ws.on('message', () => ws.close(1008, 'This channel only sends mod notifications'));
        }
      });
    } catch (error) {
      const status = error instanceof HttpError ? error.status : 400;
      socket.end(`HTTP/1.1 ${status} Rejected\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`);
    }
  });
  const heartbeat = setInterval(() => {
    for (const ws of wss.clients) {
      if (!ws.alive || !friends.get(sockets.get(ws))) ws.terminate();
      else { ws.alive = false; ws.ping(); }
    }
  }, 30_000); heartbeat.unref();
  return { server, store, config,
    async close() {
      clearInterval(heartbeat);
      for (const ws of wss.clients) ws.terminate();
      await new Promise(resolveClose => wss.close(resolveClose));
      server.closeAllConnections();
      if (server.listening) await new Promise((resolveClose, reject) => server.close(error => error ? reject(error) : resolveClose()));
    },
  };
}
