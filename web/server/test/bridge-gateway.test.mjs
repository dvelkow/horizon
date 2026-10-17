import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { createServer as tcpServer } from 'node:net';
import { once } from 'node:events';
import { request as httpRequest } from 'node:http';
import WebSocket from 'ws';
import { createGateway } from '../gateway.mjs';
import { hashSecret } from '../security.mjs';

const invite = 'test-invite-with-enough-entropy';
const inviteHash = await hashSecret(invite);
const origin = 'https://game.example.test';

async function fixture(t, overrides = {}) {
  const root = await fs.mkdtemp(join(tmpdir(), 'horizon-gateway-'));
  for (const name of ['public', 'assets', 'runtime']) await fs.mkdir(join(root, name));
  await fs.writeFile(join(root, 'public', 'index.html'), '<!doctype html><title>Horizon</title>');
  await fs.writeFile(join(root, 'assets', 'example.bin'), 'asset');
  const config = { origin, inviteHash, privateDir: join(root, 'private'), publicDir: join(root, 'public'),
    assetsDir: join(root, 'assets'), modsRuntimeDir: join(root, 'runtime'), gamePort: 25565,
    ...overrides };
  const gateway = await createGateway(config);
  await new Promise(resolve => gateway.server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${gateway.server.address().port}`;
  t.after(async () => { await gateway.close(); await fs.rm(root, { recursive: true, force: true }); });
  const request = (path, options = {}) => new Promise((resolveResponse, reject) => {
    const request = httpRequest(`${base}${path}`, { method: options.method ?? 'GET',
      headers: { Host: 'game.example.test', Origin: origin,
        ...(options.body ? { 'Content-Length': Buffer.byteLength(options.body) } : {}), ...options.headers } }, response => {
      const chunks = [];
      response.on('data', chunk => chunks.push(chunk));
      response.on('end', () => resolveResponse(new Response(Buffer.concat(chunks), { status: response.statusCode, headers: response.headers })));
    });
    request.on('error', reject); request.end(options.body);
  });
  const login = async (name = 'Player') => {
    const response = await request('/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({invite, name}) });
    assert.equal(response.status, 200);
    return { cookie: response.headers.get('set-cookie').split(';')[0], ...(await response.json()) };
  };
  const websocket = (path, session, extra = {}) => new WebSocket(`${base.replace('http:', 'ws:')}${path}`, {
    headers: { Host: 'game.example.test', Cookie: session?.cookie ?? '', Origin: origin, ...extra }, perMessageDeflate: false });
  return { root, config, gateway, request, login, websocket };
}

function vint(value) { const bytes = []; do { const part = value & 127; value >>>= 7; bytes.push(part | (value ? 128 : 0)); } while (value); return Buffer.from(bytes); }
function str(text) { const bytes = Buffer.from(text); return Buffer.concat([vint(bytes.length), bytes]); }
function framed(bytes) { return Buffer.concat([vint(bytes.length), bytes]); }
function hello(name) {
  const uuid = createHash('md5').update(`OfflinePlayer:${name}`).digest(); uuid[6] = (uuid[6] & 15) | 48; uuid[8] = (uuid[8] & 63) | 128;
  const handshake = framed(Buffer.concat([vint(0), vint(777), str('ignored-browser-target'), Buffer.from([0x63, 0xdd]), vint(2)]));
  const login = framed(Buffer.concat([vint(0), str(name), uuid]));
  return Buffer.concat([handshake, login]);
}

test('game bridge forwards a fragmented authenticated stream only to fixed loopback upstream', async t => {
  let received = Buffer.alloc(0), connections = 0;
  const upstream = tcpServer(socket => { connections++; socket.on('data', bytes => { received = Buffer.concat([received, bytes]); socket.write(bytes); }); });
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => upstream.close(resolve)));
  const f = await fixture(t, { gamePort: upstream.address().port, handshakeTimeoutMs: 150 }), friend = await f.login();
  const ws = f.websocket('/game', friend); await once(ws, 'open');
  const bytes = hello('Player'), echoed = once(ws, 'message');
  ws.send(bytes.subarray(0, 2)); ws.send(bytes.subarray(2));
  assert.deepEqual((await echoed)[0], bytes); assert.deepEqual(received, bytes); assert.equal(connections, 1);
  ws.terminate();
  const wrong = f.websocket('/game', friend); await once(wrong, 'open');
  const closed = once(wrong, 'close'); wrong.send(hello('Owner'));
  assert.equal((await closed)[0], 1008); assert.equal(connections, 1);
  const stalled = f.websocket('/game', friend); await once(stalled, 'open');
  assert.equal((await once(stalled, 'close'))[0], 1008);
});

test('WebSocket upgrades reject wrong origins, unauthenticated access and destination parameters', async t => {
  const f = await fixture(t), friend = await f.login();
  for (const [path, session, headers, expected] of [
    ['/game', null, {}, 401], ['/game', friend, { Origin: 'https://attacker.test' }, 403], ['/game?host=elsewhere', friend, {}, 400],
  ]) {
    const ws = f.websocket(path, session, headers);
    const status = await new Promise(resolve => { ws.on('unexpected-response', (_request, response) => { resolve(response.statusCode); response.resume(); ws.terminate(); }); ws.on('error', () => {}); });
    assert.equal(status, expected);
  }
  const ws = f.websocket('/game', friend); await once(ws, 'open');
  const closed = once(ws, 'close');
  assert.equal((await f.request('/api/logout', { method: 'POST', headers: { Cookie: friend.cookie, 'X-CSRF-Token': friend.csrf } })).status, 200);
  assert.equal((await closed)[0], 1008);
});

test('friend sessions protect assets and configuration stays outside public roots', async t => {
  const f = await fixture(t);
  const page = await f.request('/'); assert.equal(page.status, 200);
  assert.equal(page.headers.get('cross-origin-opener-policy'), 'same-origin');
  assert.equal(page.headers.get('cross-origin-embedder-policy'), 'require-corp');
  assert.equal((await f.request('/assets/example.bin')).status, 401);
  const session = await f.login();
  assert.equal((await f.request('/assets/example.bin', {headers: {Cookie: session.cookie}})).status, 200);
  assert.equal((await f.request('/api/logout', {method: 'POST', headers: {Cookie: session.cookie}})).status, 403);
  await fs.writeFile(join(f.root, 'private', 'secret'), 'private sentinel');
  await fs.symlink(join(f.root, 'private', 'secret'), join(f.root, 'public', 'secret'));
  assert.equal((await f.request('/secret')).status, 404);
  assert.equal((await f.request('/api/mods/upload', {method: 'POST'})).status, 405);
});
