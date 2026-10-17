import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { request as httpRequest } from 'node:http';
import { once } from 'node:events';
import WebSocket from 'ws';
import { createGateway } from '../gateway.mjs';
import { hashSecret } from '../security.mjs';
import { sha256 } from '../mod-store.mjs';

// Opt-in: these are actual native Wasmtime/Jco validation tests, not the fixture.
const validator = process.env.HORIZON_REAL_VALIDATOR;
const repo = fileURLToPath(new URL('../../../', import.meta.url));

test('real coords-hud and server guest pass quarantine, owner approval, artifact integrity and revocation',
  { skip: !validator, timeout: 90_000 }, async t => {
    const root = await fs.mkdtemp(join(tmpdir(), 'horizon-real-portal-'));
    for (const name of ['public', 'assets', 'runtime']) await fs.mkdir(join(root, name));
    await fs.writeFile(join(root, 'public', 'index.html'), '<!doctype html><title>Integration</title>');
    const invite = 'integration-only-friend-passphrase', secret = 'separate-integration-owner-passphrase';
    const origin = 'https://game.example.test';
    const gateway = await createGateway({ origin, inviteHash: await hashSecret(invite), adminHash: await hashSecret(secret),
      privateDir: join(root, 'private'), publicDir: join(root, 'public'), assetsDir: join(root, 'assets'),
      modsRuntimeDir: join(root, 'runtime'), gamePort: 25565, validatorExecutable: validator,
      jcoExecutable: process.env.HORIZON_REAL_JCO ?? join(repo, 'web/mods/node_modules/.bin/jco'),
    });
    await new Promise(resolve => gateway.server.listen(0, '127.0.0.1', resolve));
    t.after(async () => { await gateway.close(); await fs.rm(root, { recursive: true, force: true }); });
    const port = gateway.server.address().port;
    const request = (path, { method = 'GET', body, session, contentType = 'application/json' } = {}) => new Promise((resolve, reject) => {
      const req = httpRequest({ host: '127.0.0.1', port, path, method, headers: { Host: 'game.example.test', Origin: origin,
        ...(body ? { 'Content-Length': body.length, 'Content-Type': contentType } : {}),
        ...(session ? { Cookie: session.cookie, 'X-CSRF-Token': session.csrf } : {}),
      } }, res => {
        const chunks = []; res.on('data', value => chunks.push(value));
        res.on('end', () => resolve({ status: res.statusCode, headers: res.headers, bytes: Buffer.concat(chunks) }));
      });
      req.on('error', reject); req.end(body);
    });
    const json = (path, value, session) => request(path, { method: 'POST', body: Buffer.from(JSON.stringify(value)), session });
    const login = async owner => {
      const response = await json(owner ? '/api/admin/login' : '/api/login', owner ? { secret } : { invite, name: 'Integration' });
      assert.equal(response.status, 200);
      assert.match(response.headers['set-cookie'][0], /HttpOnly; SameSite=Strict; Max-Age=\d+; Secure$/);
      return { ...JSON.parse(response.bytes), cookie: response.headers['set-cookie'][0].split(';')[0] };
    };
    const friend = await login(false), owner = await login(true);
    const ws = new WebSocket(`ws://127.0.0.1:${port}/mods/events`, { headers: { Host: 'game.example.test', Origin: origin, Cookie: friend.cookie } });
    const notifications = []; ws.on('message', bytes => notifications.push(JSON.parse(bytes)));
    await once(ws, 'open');
    const inputs = [
      process.env.HORIZON_COORDS_PACKAGE ?? join(repo, 'target/mods-test/mods/coords_hud.hmod'),
      process.env.HORIZON_SERVER_PACKAGE ?? join(repo, 'target/mods-test/mods/server-side/test_guest.hmod'),
    ];
    for (const [position, path] of inputs.entries()) {
      const bytes = await fs.readFile(path), hash = sha256(bytes);
      const upload = await request('/api/mods/upload', { method: 'POST', body: bytes, session: friend, contentType: 'application/octet-stream' });
      assert.equal(upload.status, 202, upload.bytes.toString());
      assert.equal(JSON.parse(upload.bytes).status, 'pending');
      assert.equal(JSON.parse(upload.bytes).sha256, hash);
      assert.equal((await request(`/mods/packages/${hash}.hmod`, { session: friend })).status, 404);
      const pending = JSON.parse((await request('/api/mods/pending', { session: owner })).bytes).candidates;
      const candidate = pending.find(value => value.sha256 === hash);
      assert.ok(candidate); assert.equal(candidate.checks.sandboxInitFuel, true); assert.equal(candidate.checks.imports, true);
      assert.equal((await json(`/api/mods/${hash}/approve`, { sha256: hash }, friend)).status, 401);
      const approved = await json(`/api/mods/${hash}/approve`, { sha256: hash }, owner);
      assert.equal(approved.status, 200, approved.bytes.toString());
      const index = JSON.parse((await request('/mods/index.json', { session: friend })).bytes);
      assert.equal(index.revision, position + 1);
      const mod = index.mods.find(value => value.sha256 === hash); assert.ok(mod);
      assert.equal(sha256((await request(mod.url, { session: friend })).bytes), hash);
      if (position === 0) {
        assert.equal(mod.manifest.id, 'coords_hud'); assert.ok(mod.files.length >= 2);
        assert.equal(candidate.checks.browserMemoryCap, 64 << 20);
        for (const file of mod.files) {
          const artifact = await request(file.url, { session: friend });
          assert.equal(artifact.status, 200); assert.equal(artifact.bytes.length, file.size); assert.equal(sha256(artifact.bytes), file.sha256);
        }
      } else {
        assert.equal(mod.manifest.side, 'server'); assert.equal(mod.entry, null); assert.deepEqual(mod.files, []);
        const mirror = join(root, 'private', 'approved-server', `${mod.id}.hmod`);
        assert.equal(sha256(await fs.readFile(mirror)), hash);
        assert.equal((await json(`/api/mods/${hash}/revoke`, { sha256: hash }, owner)).status, 200);
        await assert.rejects(fs.stat(mirror), { code: 'ENOENT' });
      }
    }
    // A rejected native parse cannot alter the active index.
    const rejected = await request('/api/mods/upload', { method: 'POST', body: Buffer.from('not a mod archive'), session: friend, contentType: 'application/octet-stream' });
    assert.equal(rejected.status, 422);
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.deepEqual(notifications.map(value => value.revision), [0, 1, 2, 3]);
    assert.equal(gateway.store.index.mods.length, 1);
    ws.terminate();
  });
