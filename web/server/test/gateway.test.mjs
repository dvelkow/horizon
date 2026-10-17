import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { createServer as tcpServer } from 'node:net';
import { once } from 'node:events';
import { request as httpRequest } from 'node:http';
import WebSocket from 'ws';
import { createGateway } from '../gateway.mjs';
import { hashSecret } from '../security.mjs';
import { ModStore, sha256 } from '../mod-store.mjs';

const invite = 'test-invite-with-enough-entropy', secret = 'different-owner-test-secret';
const inviteHash = await hashSecret(invite), adminHash = await hashSecret(secret);
const origin = 'https://game.example.test';

async function fixture(t, overrides = {}) {
  const root = await fs.mkdtemp(join(tmpdir(), 'horizon-gateway-'));
  for (const name of ['public', 'assets', 'runtime']) await fs.mkdir(join(root, name));
  await fs.writeFile(join(root, 'public', 'index.html'), '<!doctype html><title>Horizon</title>');
  await fs.writeFile(join(root, 'assets', 'example.bin'), 'asset');
  const config = { origin, inviteHash, adminHash, privateDir: join(root, 'private'), publicDir: join(root, 'public'),
    assetsDir: join(root, 'assets'), modsRuntimeDir: join(root, 'runtime'), gamePort: 25565,
    validatorExecutable: process.execPath, validatorArgs: [fileURLToPath(new URL('./fixture-validator.mjs', import.meta.url))],
    validatorResourceLimits: false, validatorTimeoutMs: 2000, ...overrides };
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
  const login = async (owner = false, name = 'Player') => {
    const response = await request(owner ? '/api/admin/login' : '/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(owner ? { secret } : { invite, name }) });
    assert.equal(response.status, 200);
    return { cookie: response.headers.get('set-cookie').split(';')[0], ...(await response.json()) };
  };
  const upload = async (session, content = {}) => {
    const bytes = Buffer.from(JSON.stringify(content));
    const response = await request('/api/mods/upload', { method: 'POST', headers: {
      Cookie: session.cookie, 'X-CSRF-Token': session.csrf, 'Content-Type': 'application/octet-stream' }, body: bytes });
    return { response, candidate: await response.json(), bytes };
  };
  const approve = (session, hash, expected = hash) => request(`/api/mods/${hash}/approve`, { method: 'POST', headers: {
    Cookie: session.cookie, 'X-CSRF-Token': session.csrf, 'Content-Type': 'application/json' }, body: JSON.stringify({ sha256: expected }) });
  const websocket = (path, session, extra = {}) => new WebSocket(`${base.replace('http:', 'ws:')}${path}`, {
    headers: { Host: 'game.example.test', Cookie: session?.cookie ?? '', Origin: origin, ...extra }, perMessageDeflate: false });
  return { root, config, gateway, request, login, upload, approve, websocket };
}

test('authentication, origin, CSRF and static roots enforce separate boundaries', async t => {
  const f = await fixture(t);
  const page = await f.request('/');
  assert.equal(page.status, 200);
  assert.equal(page.headers.get('cross-origin-opener-policy'), 'same-origin');
  assert.equal(page.headers.get('cross-origin-embedder-policy'), 'require-corp');
  assert.match(page.headers.get('content-security-policy'), /'wasm-unsafe-eval'/);
  assert.doesNotMatch(page.headers.get('content-security-policy'), /'unsafe-eval'/);
  assert.equal((await f.request('/assets/example.bin')).status, 401);
  assert.equal((await f.request('/api/mods/pending')).status, 401);
  assert.equal((await f.request('/', { headers: { Host: 'attacker.test' } })).status, 421);
  assert.equal((await f.request('/api/login', { method: 'POST', headers: { Origin: 'https://attacker.test', 'Content-Type': 'application/json' }, body: JSON.stringify({ invite, name: 'Player' }) })).status, 403);
  const friend = await f.login();
  assert.equal((await f.request('/assets/example.bin', { headers: { Cookie: friend.cookie } })).status, 200);
  assert.equal((await f.request('/api/mods/upload', { method: 'POST', headers: { Cookie: friend.cookie, 'Content-Type': 'application/octet-stream' }, body: '{}' })).status, 403);
  assert.equal((await f.request('/api/admin/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ secret: invite }) })).status, 401);
  await fs.writeFile(join(f.root, 'private', 'provisioning-secret'), 'must-never-be-served');
  await fs.symlink(join(f.root, 'private', 'provisioning-secret'), join(f.root, 'public', 'leak.txt'));
  assert.equal((await f.request('/leak.txt')).status, 404);
  assert.equal((await f.request('/private/provisioning-secret')).status, 404);
  assert.equal((await f.request('/assets/%2e%2e/private/provisioning-secret', { headers: { Cookie: friend.cookie } })).status, 404);
});

test('upload is quarantined until exact-hash owner approval and notifications follow publication', async t => {
  const f = await fixture(t), friend = await f.login(), owner = await f.login(true);
  process.env.DO_TOKEN = 'test-sentinel-not-a-real-credential';
  t.after(() => { delete process.env.DO_TOKEN; });
  const { response, candidate, bytes } = await f.upload(friend);
  assert.equal(response.status, 202); assert.equal(candidate.status, 'pending'); assert.equal(candidate.sha256, sha256(bytes));
  const index = await (await f.request('/mods/index.json', { headers: { Cookie: friend.cookie } })).json();
  assert.equal(index.revision, 0); assert.deepEqual(index.mods, []);
  assert.equal((await f.request(`/mods/packages/${candidate.sha256}.hmod`, { headers: { Cookie: friend.cookie } })).status, 404);
  assert.equal((await f.request(`/mods/bundles/${candidate.sha256}/mod.js`, { headers: { Cookie: friend.cookie } })).status, 404);
  assert.equal((await f.approve(friend, candidate.sha256)).status, 401);
  assert.equal((await f.approve(owner, candidate.sha256, '0'.repeat(64))).status, 409);
  const pending = await (await f.request('/api/mods/pending', { headers: { Cookie: owner.cookie } })).json();
  assert.equal(pending.candidates[0].checks.cloudCredentialInEnvironment, false);
  const ws = f.websocket('/mods/events', friend), notifications = [];
  ws.on('message', bytes => notifications.push(JSON.parse(bytes)));
  await once(ws, 'open');
  assert.equal((await f.approve(owner, candidate.sha256)).status, 200);
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.ok(notifications.some(value => value.type === 'mods-changed' && value.revision === 1));
  const published = await (await f.request('/mods/index.json', { headers: { Cookie: friend.cookie } })).json();
  assert.equal(published.revision, 1); assert.equal(published.mods[0].manifest.authors[0], 'Test author');
  const packageBytes = Buffer.from(await (await f.request(published.mods[0].url, { headers: { Cookie: friend.cookie } })).arrayBuffer());
  assert.equal(sha256(packageBytes), candidate.sha256);
  const file = published.mods[0].files[0];
  assert.equal(sha256(Buffer.from(await (await f.request(file.url, { headers: { Cookie: friend.cookie } })).arrayBuffer())), file.sha256);
  assert.equal((await f.approve(owner, candidate.sha256)).status, 200);
  assert.equal(f.gateway.store.index.revision, 1);
  const update = await f.upload(friend, { version: '2.0.0' });
  assert.equal((await f.approve(owner, update.candidate.sha256)).status, 200);
  assert.equal(f.gateway.store.index.mods.length, 1);
  assert.equal((await f.request(file.url, { headers: { Cookie: friend.cookie } })).status, 200);
  ws.terminate();
});

test('rejected, oversized, modified and registry candidates never enter the index', async t => {
  const f = await fixture(t, { maxUploadBytes: 1024 }), friend = await f.login(), owner = await f.login(true);
  for (const content of [{ reject: true }, { badHash: true }]) {
    const result = await f.upload(friend, content);
    assert.equal(result.response.status, 422); assert.equal(result.candidate.status, 'rejected');
  }
  const oversized = await f.request('/api/mods/upload', { method: 'POST', headers: { Cookie: friend.cookie, 'X-CSRF-Token': friend.csrf, 'Content-Type': 'application/octet-stream' }, body: Buffer.alloc(1025) });
  assert.equal(oversized.status, 413);
  const modified = await f.upload(friend, { id: 'modified' });
  await fs.chmod(join(f.root, 'private', 'candidates', modified.candidate.sha256, 'browser', 'mod.js'), 0o600);
  await fs.writeFile(join(f.root, 'private', 'candidates', modified.candidate.sha256, 'browser', 'mod.js'), 'tampered');
  assert.equal((await f.approve(owner, modified.candidate.sha256)).status, 409);
  const registry = await f.upload(friend, { id: 'registry', capabilities: ['registries'] });
  assert.equal(registry.response.status, 202);
  assert.equal((await f.approve(owner, registry.candidate.sha256)).status, 409);
  assert.equal(f.gateway.store.index.revision, 0);
});

test('server and both mods mirror approved bytes, recover from interruption, and revoke explicitly', async t => {
  const f = await fixture(t), friend = await f.login(), owner = await f.login(true);
  const server = await f.upload(friend, { id: 'server_only', side: 'server' });
  const directory = join(f.root, 'private', 'approved-server');
  assert.deepEqual(await fs.readdir(directory), []);
  assert.equal((await f.approve(owner, server.candidate.sha256)).status, 200);
  assert.deepEqual(await fs.readFile(join(directory, 'server_only.hmod')), server.bytes);
  assert.equal(f.gateway.store.index.mods[0].entry, null);
  const both = await f.upload(friend, { id: 'both', side: 'both' });
  assert.equal((await f.approve(owner, both.candidate.sha256)).status, 200);
  assert.deepEqual(await fs.readFile(join(directory, 'both.hmod')), both.bytes);
  await fs.unlink(join(directory, 'server_only.hmod'));
  await fs.writeFile(join(directory, 'orphan.hmod'), 'withdrawn');
  const recovered = new ModStore(f.config); await recovered.init();
  assert.deepEqual(await fs.readFile(join(directory, 'server_only.hmod')), server.bytes);
  assert.deepEqual((await fs.readdir(directory)).sort(), ['both.hmod', 'server_only.hmod']);
  const response = await f.request(`/api/mods/${server.candidate.sha256}/revoke`, { method: 'POST', headers: {
    Cookie: owner.cookie, 'X-CSRF-Token': owner.csrf, 'Content-Type': 'application/json' }, body: JSON.stringify({ sha256: server.candidate.sha256 }) });
  assert.equal(response.status, 200); assert.deepEqual(await fs.readdir(directory), ['both.hmod']);
  assert.equal(f.gateway.store.index.mods.length, 1);
  assert.equal((await f.request(`/mods/packages/${server.candidate.sha256}.hmod`, { headers: { Cookie: friend.cookie } })).status, 200);
});

test('validation deadline rejects hanging uploads and completed candidates release their staging quota', async t => {
  const f = await fixture(t, { validatorTimeoutMs: 150, maxCandidates: 1 }), friend = await f.login(), owner = await f.login(true);
  const hung = await f.upload(friend, { hang: true });
  assert.equal(hung.response.status, 422); assert.match(hung.candidate.error, /timed out/);
  assert.deepEqual(await fs.readdir(join(f.root, 'private', 'candidates', hung.candidate.sha256)), ['candidate.json']);
  const first = await f.upload(friend, { id: 'first' });
  assert.equal((await f.approve(owner, first.candidate.sha256)).status, 200);
  assert.deepEqual(await fs.readdir(join(f.root, 'private', 'candidates', first.candidate.sha256)), ['candidate.json']);
  assert.equal((await f.upload(friend, { id: 'second' })).response.status, 202);
  const nullDecision = await f.request(`/api/mods/${first.candidate.sha256}/approve`, { method: 'POST', headers: {
    Cookie: owner.cookie, 'X-CSRF-Token': owner.csrf, 'Content-Type': 'application/json' }, body: 'null' });
  assert.equal(nullDecision.status, 400);
});

test('approval serializes concurrent candidates without losing an index update', async t => {
  const f = await fixture(t), friend = await f.login(), owner = await f.login(true);
  const a = await f.upload(friend, { id: 'first' }), b = await f.upload(friend, { id: 'second' });
  const results = await Promise.all([f.approve(owner, a.candidate.sha256), f.approve(owner, b.candidate.sha256)]);
  assert.deepEqual(results.map(value => value.status), [200, 200]);
  assert.equal(f.gateway.store.index.revision, 2); assert.equal(f.gateway.store.index.mods.length, 2);
});

test('the active mod limit rejects new IDs while allowing an existing ID update', async t => {
  const f = await fixture(t, { maxActiveMods: 1 }), friend = await f.login(), owner = await f.login(true);
  const original = await f.upload(friend, { id: 'first' });
  assert.equal((await f.approve(owner, original.candidate.sha256)).status, 200);
  const other = await f.upload(friend, { id: 'second' });
  assert.equal((await f.approve(owner, other.candidate.sha256)).status, 409);
  const replacement = await f.upload(friend, { id: 'first', version: '2.0.0' });
  assert.equal((await f.approve(owner, replacement.candidate.sha256)).status, 200);
  assert.equal(f.gateway.store.index.mods.length, 1);
  assert.equal(f.gateway.store.index.mods[0].version, '2.0.0');
});

test('only eight browser mods may be active, while server-only mods use separate slots', async t => {
  const f = await fixture(t), friend = await f.login(), owner = await f.login(true);
  for (let i = 0; i < 8; i++) {
    const upload = await f.upload(friend, { id: `client_${i}` });
    assert.equal((await f.approve(owner, upload.candidate.sha256)).status, 200);
  }
  const ninth = await f.upload(friend, { id: 'ninth', side: 'both' });
  assert.equal((await f.approve(owner, ninth.candidate.sha256)).status, 409);
  const server = await f.upload(friend, { id: 'server_only', side: 'server' });
  assert.equal((await f.approve(owner, server.candidate.sha256)).status, 200);
  const replacement = await f.upload(friend, { id: 'client_0', version: '2.0.0' });
  assert.equal((await f.approve(owner, replacement.candidate.sha256)).status, 200);
  const changeSide = await f.upload(friend, { id: 'server_only', side: 'both', version: '2.0.0' });
  assert.equal((await f.approve(owner, changeSide.candidate.sha256)).status, 409);
  assert.equal(f.gateway.store.index.mods.length, 9);
  assert.equal(f.gateway.store.index.mods.filter(mod => mod.side !== 'server').length, 8);
});

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
  const ws = f.websocket('/mods/events', friend); await once(ws, 'open');
  const closed = once(ws, 'close');
  assert.equal((await f.request('/api/logout', { method: 'POST', headers: { Cookie: friend.cookie, 'X-CSRF-Token': friend.csrf } })).status, 200);
  assert.equal((await closed)[0], 1008);
});
