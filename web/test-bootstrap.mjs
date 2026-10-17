import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import test from 'node:test';
import {createHash} from 'node:crypto';

const source = await readFile(new URL('./bootstrap.js', import.meta.url), 'utf8');
let serial = 0;
async function browser(t) {
  const listeners = new Map(), elements = new Map();
  const calls = [];
  const canvas = {
    requestPointerLock: async () => { calls.push('pointer'); },
    requestFullscreen: async () => { calls.push('fullscreen'); },
    focus: () => { calls.push('focus'); },
  };
  elements.set('horizon', canvas);
  const document = {
    pointerLockElement: null,
    fullscreenElement: null,
    hasFocus: () => true,
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
  return {api, lock, opened, listeners, canvas, document, calls};
}
const settled = () => new Promise(resolve => setImmediate(resolve));

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

test('Escape resumes mouse control after a long pause without a click or active gesture', async t => {
  const {api, lock, calls} = await browser(t);
  navigator.userActivation.isActive = true;
  api.setPointerCapture(true); lock(true); await settled();
  api.setPointerCapture(false);
  navigator.userActivation.isActive = false;
  assert.equal(api.setPointerCapture(true), false, 'capture stays false until the browser grants it');
  assert.deepEqual(calls, ['focus', 'pointer', 'focus', 'pointer']);
  lock(true); await settled();
  assert.equal(api.pointerCaptured(), true);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('several Escape pause/resume cycles all recapture without new clicks', async t => {
  const {api, lock, calls} = await browser(t);
  for (let i = 0; i < 5; i++) {
    api.setPointerCapture(true); lock(true); await settled();
    assert.equal(api.pointerCaptured(), true);
    api.setPointerCapture(false);
    assert.equal(api.takePointerPauseRequest(), false);
  }
  assert.equal(calls.filter(call => call === 'pointer').length, 5);
});

test('a denied lock waits for fresh keyboard input instead of retrying every frame', async t => {
  const {api, listeners, canvas, calls} = await browser(t);
  canvas.requestPointerLock = async () => { calls.push('pointer'); throw new Error('NotAllowedError'); };
  api.setPointerCapture(true); await settled();
  for (let i = 0; i < 100; i++) api.setPointerCapture(true);
  for (const event of [{isTrusted:false}, {isTrusted:true, repeat:true}, {isTrusted:true, target:{}}]) {
    listeners.get('keydown')({target:canvas, repeat:false, ...event}); api.setPointerCapture(true);
  }
  assert.equal(calls.filter(call => call === 'pointer').length, 1);
  listeners.get('keydown')({isTrusted:true, repeat:false, target:canvas, code:'Escape'});
  api.setPointerCapture(true); await settled();
  assert.equal(calls.filter(call => call === 'pointer').length, 2);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('a canvas click retries a browser-denied request', async t => {
  const {api, listeners, canvas, calls} = await browser(t);
  api.setPointerCapture(true); await settled();
  listeners.get('pointerdown')({isTrusted:true, target:canvas});
  api.setPointerCapture(true); await settled();
  assert.equal(calls.filter(call => call === 'pointer').length, 2);
});

test('resuming cannot steal focus from another tab or application', async t => {
  const {api, document, calls} = await browser(t);
  document.hasFocus = () => false;
  api.setPointerCapture(true); await settled();
  assert.deepEqual(calls, []);
});

test('an in-flight lock is requested once and released if the menu opens before it arrives', async t => {
  const {api, canvas, lock, calls} = await browser(t);
  let finish;
  canvas.requestPointerLock = () => { calls.push('pointer'); return new Promise(resolve => { finish = resolve; }); };
  api.setPointerCapture(true);
  api.setPointerCapture(false); api.setPointerCapture(true);
  assert.equal(calls.filter(call => call === 'pointer').length, 1);
  api.setPointerCapture(false); lock(true); finish(); await settled();
  assert.equal(api.pointerCaptured(), false);
  assert.equal(api.takePointerPauseRequest(), false);
});

test('first play gesture requests pointer before fullscreen and captures only Escape', async t => {
  const {api, canvas, document, listeners, calls, lock} = await browser(t);
  navigator.userActivation.isActive = true;
  const keys = [];
  navigator.keyboard = {lock: async value => keys.push(value), unlock: () => keys.push('unlock')};
  api.setPointerCapture(true);
  assert.deepEqual(calls, ['focus', 'pointer', 'fullscreen']);
  document.fullscreenElement = canvas; listeners.get('fullscreenchange')();
  lock(true); await settled(); api.setPointerCapture(false);
  assert.deepEqual(keys, ['unlock', ['Escape']], 'keyboard lock stays through the menu');
  document.fullscreenElement = null; listeners.get('fullscreenchange')();
  assert.deepEqual(keys, ['unlock', ['Escape'], 'unlock']);
  api.setPointerCapture(true); await settled();
  assert.equal(calls.filter(call => call === 'fullscreen').length, 1, 'deliberate exit is respected');
});

test('without activation fullscreen waits for a real gesture', async t => {
  const {api, canvas, listeners, calls} = await browser(t);
  navigator.keyboard = {lock: async () => {}, unlock: () => {}};
  api.setPointerCapture(true); await settled();
  assert.equal(calls.includes('fullscreen'), false);
  navigator.userActivation.isActive = true;
  listeners.get('pointerdown')({isTrusted:true, target:canvas});
  api.setPointerCapture(true); await settled();
  assert.equal(calls.filter(call => call === 'fullscreen').length, 1);
});

test('leaving fullscreen entered with F11 is also respected on resume', async t => {
  const {api, canvas, document, listeners, calls} = await browser(t);
  navigator.keyboard = {lock: async () => {}, unlock: () => {}};
  api.setPointerCapture(false);
  document.fullscreenElement = canvas; listeners.get('fullscreenchange')(); await settled();
  document.fullscreenElement = null; listeners.get('fullscreenchange')();
  navigator.userActivation.isActive = true;
  api.setPointerCapture(true); await settled();
  assert.equal(calls.includes('fullscreen'), false);
});

test('denying fullscreen or Escape permission does not break windowed controls', async t => {
  const {api, canvas, document, listeners, lock} = await browser(t);
  navigator.userActivation.isActive = true;
  navigator.keyboard = {lock: async () => { throw new Error('denied'); }, unlock: () => {}};
  canvas.requestFullscreen = async () => { throw new Error('denied'); };
  api.setPointerCapture(true); lock(true); await settled();
  assert.equal(api.pointerCaptured(), true);
  document.fullscreenElement = canvas; listeners.get('fullscreenchange')(); await settled();
  api.setPointerCapture(false); api.setPointerCapture(true); lock(true); await settled();
  assert.equal(api.pointerCaptured(), true);
});

test('leaving fullscreen while keyboard permission is pending releases a late lock', async t => {
  const {api, canvas, document, listeners} = await browser(t);
  let finish, unlocks = 0;
  navigator.keyboard = {lock: () => new Promise(resolve => { finish = resolve; }), unlock: () => unlocks++};
  api.setPointerCapture(false);
  document.fullscreenElement = canvas; listeners.get('fullscreenchange')();
  document.fullscreenElement = null; listeners.get('fullscreenchange')();
  const before = unlocks; finish(); await settled();
  assert.equal(unlocks, before + 1);
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

test('linked skins load on demand from this host and require the matching SHA-256', async t => {
  const {api} = await browser(t), bytes = new Uint8Array([1,2,3,4]);
  const hash=createHash('sha256').update(bytes).digest('hex'), path=`modcraft/skins/slim/${hash}`;
  t.mock.method(globalThis,'setInterval',()=>0); t.mock.method(globalThis,'clearInterval',()=>{});
  const calls=[];
  t.mock.method(globalThis,'fetch',async(url,options)=>{
    calls.push({url,options});
    if(url==='/api/players/skins') return Response.json({players:{['a'.repeat(32)]:{name:'Builder',skin:`mc_slim_${hash}`}}});
    return new Response(bytes,{headers:{'Content-Length':String(bytes.length)}});
  });
  await api.startPlayerSkins('Builder');assert.equal(api.selectedPlayerSkin(''),path);
  api.requestPlayerSkin('https://evil.test/skin');assert.equal(calls.length,1);
  api.requestPlayerSkin(path);api.requestPlayerSkin(path);
  let ready=[];
  for(let i=0;i<50&&!ready.length;i++){await new Promise(r=>setTimeout(r,5));ready=api.takePlayerSkinTextures();}
  assert.equal(calls.length,2);assert.equal(calls[1].url,`/player-skins/mc_slim_${hash}.png`);
  assert.equal(calls[1].options.credentials,'same-origin');assert.equal(calls[1].options.redirect,'error');
  assert.deepEqual(ready,[{path,bytes}]);assert.deepEqual(api.takePlayerSkinTextures(),[]);
  api.requestPlayerSkin(`modcraft/skins/slim/${'c'.repeat(64)}`);
  await new Promise(r=>setTimeout(r,30));assert.deepEqual(api.takePlayerSkinTextures(),[]);
});
