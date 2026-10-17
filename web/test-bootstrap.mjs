import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';

const source = await readFile(new URL('./bootstrap.js', import.meta.url), 'utf8');
let serial = 0;
async function browser(t) {
  const listeners = new Map(), elements = new Map();
  const canvas = {requestPointerLock: async () => {}};
  elements.set('horizon', canvas);
  const document = {
    pointerLockElement: null,
    getElementById: id => elements.get(id),
    addEventListener: (name, callback) => listeners.set(name, callback),
    createElement: () => ({style: {}, setAttribute() {}, remove() { elements.delete(this.id); }}),
    body: {append: element => elements.set(element.id, element)},
    exitPointerLock() { this.pointerLockElement = null; listeners.get('pointerlockchange')(); },
  };
  const opened = [];
  for (const [key, value] of Object.entries({document, navigator: {userActivation: {isActive: false}}, window: {open: (...args) => opened.push(args)}})) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, key);
    Object.defineProperty(globalThis, key, {value, configurable: true});
    t.after(() => previous ? Object.defineProperty(globalThis, key, previous) : delete globalThis[key]);
  }
  t.after(() => delete globalThis.horizonPointerCaptured);
  const api = await import(`data:text/javascript;base64,${Buffer.from(source + `\n// case ${serial++}`).toString('base64')}`);
  const lock = value => { document.pointerLockElement = value ? canvas : null; listeners.get('pointerlockchange')(); };
  return {api, lock, opened, listeners};
}

test('Chrome releasing an acquired lock requests one pause even without a key event', async t => {
  const {api, lock} = await browser(t);
  api.setPointerCapture(true);
  lock(true);
  lock(false);
  assert.equal(api.takePointerPauseRequest(), true);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('opening inventory or options deliberately releases the mouse without another pause', async t => {
  const {api, lock} = await browser(t);
  api.setPointerCapture(true);
  lock(true);
  api.setPointerCapture(false);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('a rejected initial lock never pauses the world', async t => {
  const {api, listeners} = await browser(t);
  api.setPointerCapture(true);
  listeners.get('pointerlockerror')();
  assert.equal(api.takePointerPauseRequest(), false);
});

test('a handled Escape clears a pending lock-loss pause before resuming', async t => {
  const {api, lock} = await browser(t);
  api.setPointerCapture(true);
  lock(true);
  lock(false);
  api.setPointerCapture(false);
  api.setPointerCapture(true);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('DEV MODE opens only the same-origin portal with no opener', async t => {
  const {api, opened} = await browser(t);
  api.openModPortal();
  assert.deepEqual(opened, [['/portal.html', '_blank', 'noopener,noreferrer']]);
});

test('wardrobe textures come only from validated same-origin player profiles', async t => {
  const {api} = await browser(t);
  t.mock.method(globalThis, 'setInterval', () => 0);
  t.mock.method(globalThis, 'clearInterval', () => {});
  let requested;
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    requested = {url, options};
    return Response.json({players: {
      ['a'.repeat(32)]: {name:'Builder',skin:'kai'},
      ['b'.repeat(32)]: {name:'Friend',skin:'alex'},
      ['c'.repeat(32)]: {name:'Invalid',skin:'../../secret'},
      'not-a-uuid': {name:'Invalid',skin:'steve'},
    }});
  });
  await api.startPlayerSkins('Builder');
  assert.equal(requested.url, '/api/players/skins'); assert.equal(requested.options.credentials,'same-origin');
  assert.equal(api.selectedPlayerSkin(''),'entity/player/wide/kai');
  assert.equal(api.selectedPlayerSkin('b'.repeat(32)),'entity/player/wide/alex');
  assert.equal(api.selectedPlayerSkin('c'.repeat(32)),''); assert.equal(api.selectedPlayerSkin('not-a-uuid'),'');
});
