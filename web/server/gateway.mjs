import { createServer } from 'node:http';
import * as fs from 'node:fs/promises';
import { createReadStream } from 'node:fs';
import { pipeline } from 'node:stream/promises';
import { resolve, sep, extname } from 'node:path';
import { isIP } from 'node:net';
import { WebSocketServer } from 'ws';
import { Sessions, RateLimit, validSecretHash, verifySecret, cookieValue, sessionCookie, equalToken } from './security.mjs';
import { bridgeGame } from './bridge.mjs';
import { ReadonlyModCatalog } from './readonly-mod-catalog.mjs';

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
  if (!validSecretHash(config.inviteHash)) throw new Error('A scrypt invite hash is required');
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

/** Authenticate browser players and bridge their stream to a fixed loopback server. */
export async function createGateway(input) {
  // Resolve roots before comparing them: a symlinked "public" directory must
  // never turn private state into a static asset tree.
  await fs.mkdir(input.privateDir, { recursive: true, mode: 0o700 });
  const roots = { ...input };
  for (const key of ['privateDir', 'publicDir', 'assetsDir', 'modsRuntimeDir']) if (roots[key]) roots[key] = await fs.realpath(roots[key]);
  const config = validateConfig(roots);
  const catalog = await ReadonlyModCatalog.open(config.privateDir);
  const friends = new Sessions({ capacity: config.maxSessions ?? 128 });
  const friendCookie = config.secure ? '__Host-horizon_session' : 'horizon_session';
  const logins = new RateLimit({ limit: config.loginAttempts ?? 10, windowMs: 60_000 });
  const upgrades = new RateLimit({ limit: 30, windowMs: 60_000 });
  let passwordChecks = 0;
  const sockets = new Map();
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
  const friend = request => friends.get(cookieValue(request, friendCookie)) ?? fail(401, 'Sign in with your invite first');
  const csrf = (request, session) => { origin(request); if (!equalToken(request.headers['x-csrf-token'], session.csrf)) fail(403, 'CSRF token rejected'); };
  const closeSession = id => { for (const [ws, value] of sockets) if (value === id) ws.close(1008, 'Signed out'); };

  const server = createServer(async (request, response) => {
    securityHeaders(response, config.secure);
    try {
      host(request);
      let path;
      try { path = decodeURIComponent(new URL(request.url, config.origin).pathname); } catch { fail(400, 'Invalid request path'); }
      if (request.method === 'GET' && path === '/healthz') return json(response, 200, { ok: true });
      if (request.method === 'GET' && path === '/api/session') {
        const session = friends.get(cookieValue(request, friendCookie));
        return json(response, 200, session ? { authenticated: true, name: session.name, csrf: session.csrf, expires: session.expires } : { authenticated: false });
      }
      if (request.method === 'POST' && path === '/api/login') {
        origin(request);
        if (!logins.take(`${peer(request)}:${path}`) || passwordChecks >= 2) fail(429, 'Too many sign-in attempts; retry later');
        const value = await jsonBody(request);
        passwordChecks++;
        let verified;
        try { verified = await verifySecret(value.invite, config.inviteHash); }
        finally { passwordChecks--; }
        if (!verified) fail(401, 'Sign-in rejected');
        if (typeof value.name !== 'string' || !/^[A-Za-z0-9_]{1,16}$/.test(value.name)) fail(400, 'Use a player name with 1–16 letters, digits or underscores');
        if (config.protectedNames.has(value.name.toLowerCase())) fail(403, 'That player name is reserved');
        const previous = cookieValue(request, friendCookie); friends.delete(previous); if (previous) closeSession(previous);
        const session = friends.create({name: value.name});
        return json(response, 200, {authenticated: true, name: session.name, csrf: session.csrf, expires: session.expires}, {
          'Set-Cookie': sessionCookie(friendCookie, session.id, 28800, config.secure),
        });
      }
      if (request.method === 'POST' && path === '/api/logout') {
        const session = friend(request); csrf(request, session); friends.delete(session.id); closeSession(session.id);
        return json(response, 200, {ok: true}, {'Set-Cookie': sessionCookie(friendCookie, '', 0, config.secure)});
      }
      if (request.method === 'GET' && path === '/mods/index.json') { friend(request); return json(response, 200, catalog.index); }
      if (!['GET', 'HEAD'].includes(request.method)) fail(405, 'Method not allowed');
      if (path.startsWith('/api/') || path === '/game' || path === '/mods/events') fail(404, 'Not found');
      if (path.startsWith('/mods/')) { friend(request); const file = catalog.files.get(path); if (!file) fail(404, 'Not found'); return await staticFile(request, response, config.privateDir, file, {immutable: true}); }
      if (path.startsWith('/mods-runtime/')) { friend(request); return await staticFile(request, response, config.modsRuntimeDir, path.slice('/mods-runtime/'.length)); }
      if (path.startsWith('/assets/')) { friend(request); return await staticFile(request, response, config.assetsDir, path.slice('/assets/'.length)); }
      return await staticFile(request, response, config.publicDir, path === '/' ? 'index.html' : path.slice(1));
    } catch (error) {
      if (response.headersSent || response.destroyed) { response.destroy(); return; }
      const status = error instanceof HttpError ? error.status : 500;
      json(response, status, { error: status === 500 ? 'Gateway request failed' : error.message }, { Connection: 'close' });
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
        ws.on('close', () => { sockets.delete(ws); });
        ws.on('error', () => ws.terminate());
        if (path === '/game') bridgeGame(ws, session, config);
        else { ws.send(JSON.stringify({type: 'mods-changed', revision: catalog.index.revision})); ws.on('message', () => ws.close(1008, 'Read-only mod notifications')); }
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
  return { server, config,
    async close() {
      clearInterval(heartbeat);
      for (const ws of wss.clients) ws.terminate();
      await new Promise(resolveClose => wss.close(resolveClose));
      server.closeAllConnections();
      if (server.listening) await new Promise((resolveClose, reject) => server.close(error => error ? reject(error) : resolveClose()));
    },
  };
}
