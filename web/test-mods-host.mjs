import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';
import {createHash} from 'node:crypto';

const source = await readFile(new URL('./mods/host.js', import.meta.url), 'utf8');

// web/mods/host.js in a fake page: the gateway's index and packages, no Workers.
const origin = 'https://game.example.test';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const entry = (id, {side = 'both', capabilities = ['registries'], bytes = Buffer.from(`${id} package`)} = {}) => {
  const sha256 = hash(bytes), manifest = {id, name: id, version: '0.1.0', api: '0.2', side, capabilities, description: '', authors: []};
  return {entry: {...manifest, manifest, sha256, size: bytes.length, url: `/mods/packages/${sha256}.hmod`, entry: null, files: []}, bytes};
};
let serial = 0;
async function page(t, mods, {tamper} = {}) {
  const served = new Map(mods.map(({entry, bytes}) => [entry.url, tamper === entry.id ? Buffer.from('tampered') : bytes]));
  const requests = [];
  const state = {index: {schema: 1, revision: 1, mods: mods.map(m => m.entry)}, release: '0123456789abcdef'};
  const globals = {
    location: {origin, href: `${origin}/`, reload() {}},
    document: {hidden: false},
    sessionStorage: {getItem: () => null, setItem() {}},
    addEventListener() {},
    WebSocket: class { close() {} },
    horizonOptions: {release: state.release},
    fetch: async url => {
      const path = new URL(url, origin).pathname; requests.push(path);
      if (path === '/mods/index.json') return new Response(JSON.stringify(state.index));
      if (path === '/api/release') return new Response(JSON.stringify({release: state.release}));
      return served.has(path) ? new Response(served.get(path)) : new Response('missing', {status: 404});
    },
  };
  for (const [key, value] of Object.entries(globals)) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, key);
    Object.defineProperty(globalThis, key, {value, configurable: true, writable: true});
    t.after(() => previous ? Object.defineProperty(globalThis, key, previous) : delete globalThis[key]);
  }
  t.after(() => { globalThis.horizonMods?.close(); delete globalThis.horizonMods; });
  const host = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// case ${serial++}`).toString('base64')}`);
  return {host, state, requests};
}

test('the world\'s content mods are verified and handed to the game once, by id', async t => {
  const ruby = entry('ruby'), amber = entry('amber', {side: 'client'});
  const server = entry('rules', {side: 'server', capabilities: ['chat']});
  const {host, requests} = await page(t, [ruby, server, amber]);
  const manager = await host.startMods();
  const packages = manager.takeContent();
  assert.deepEqual(packages.map(p => p.id), ['amber', 'ruby']);
  assert.deepEqual(Buffer.from(packages[1].bytes), ruby.bytes);
  assert.equal(packages[1].sha256, ruby.entry.sha256);
  assert.deepEqual(manager.takeContent(), []);
  assert.ok(requests.includes(ruby.entry.url) && !requests.includes(server.entry.url));
  // Content loaded at start is live in this page; server code is not.
  const rows = Object.fromEntries(manager.view().entries.map(e => [e.manifest.id, e]));
  assert.equal(rows.ruby.status, 'running'); assert.equal(rows.amber.status, 'running');
  assert.equal(rows.rules.status, 'off');
});

test('a content-only mod sits through the game\'s ticks, frames and chat', async t => {
  const {host} = await page(t, [entry('ruby')]);
  const manager = await host.startMods();
  manager.dispatch('snapshot', {player: {name: 'Alex'}});
  for (const method of ['tick', 'frame', 'chat']) assert.deepEqual(manager.dispatch(method, {}), {});
  assert.deepEqual(manager.dispatch('command', '/ruby'), {handled: false});
});

test('a content change waits for a reload, and a bad package stops the page joining', async t => {
  const ruby = entry('ruby');
  const {host, state, requests} = await page(t, [ruby]);
  const manager = await host.startMods();
  const next = entry('ruby', {bytes: Buffer.from('ruby 2')});
  state.index = {schema: 1, revision: 2, mods: [next.entry]};
  await manager.refresh();
  const row = manager.view().entries.find(e => e.manifest.id === 'ruby');
  assert.equal(row.status, 'off'); assert.match(row.error, /Reload the page/);
  assert.ok(requests.includes('/api/release'));
  manager.close(); delete globalThis.horizonMods;
  const broken = await page(t, [entry('zinc')], {tamper: 'zinc'});
  await assert.rejects(broken.host.startMods(), /blocks and items could not be loaded: zinc: package checksum mismatch/);
  assert.equal(globalThis.horizonMods, undefined);
});
