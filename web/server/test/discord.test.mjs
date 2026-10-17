import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { request as httpRequest } from 'node:http';
import { createHash } from 'node:crypto';
import { createGateway } from '../gateway.mjs';
import { Accounts, offlineUuid, SESSION_SECONDS } from '../accounts.mjs';
import { hashSecret } from '../security.mjs';

const origin = 'https://game.example.test', clientId = '123456789012345678', clientSecret = 'test-only-discord-secret-not-real';
const firstId = '223456789012345678', secondId = '323456789012345678';
const adminHash = await hashSecret('test-only-separate-owner-key');
async function fixture(t) {
  const root = await fs.mkdtemp(join(tmpdir(), 'modcraft-discord-'));
  for (const folder of ['public', 'assets', 'runtime']) await fs.mkdir(join(root, folder));
  await fs.writeFile(join(root, 'assets', 'test.txt'), 'protected');
  const exchanges = [], users = [];
  const discordFetch = async (url, options) => {
    if (url.endsWith('/oauth2/token')) {
      const form = new URLSearchParams(options.body); exchanges.push(form);
      assert.equal(form.get('client_secret'), clientSecret);
      if (form.get('code') === 'broken') return new Response('{}', { status: 400 });
      return Response.json({ access_token: form.get('code'), token_type: 'Bearer', scope: 'identify' });
    }
    assert.equal(url, 'https://discord.com/api/v10/users/@me');
    const id = options.headers.Authorization.slice(7); users.push(id);
    return Response.json({ id, username: 'DiscordUser', global_name: 'A Friendly Person' });
  };
  const config = { origin, adminHash, discord: { clientId, clientSecret }, protectedNames: ['Host'], privateDir: join(root, 'private'), publicDir: join(root, 'public'), assetsDir: join(root, 'assets'), modsRuntimeDir: join(root, 'runtime'), gamePort: 25565 };
  let gateway, port;
  const start = async () => { gateway = await createGateway(config, { discordFetch }); await new Promise(resolve => gateway.server.listen(0, '127.0.0.1', resolve)); port = gateway.server.address().port; };
  await start();
  t.after(async () => { await gateway.close(); await fs.rm(root, {recursive:true, force:true}); });
  const request = (path, { method = 'GET', cookie, body, csrf, originHeader = origin } = {}) => new Promise((resolve, reject) => {
    const bytes = body === undefined ? null : JSON.stringify(body);
    const req = httpRequest(`http://127.0.0.1:${port}${path}`, { method, headers: { Host: 'game.example.test', ...(originHeader ? { Origin: originHeader } : {}), ...(cookie ? {Cookie:cookie} : {}), ...(csrf ? {'X-CSRF-Token':csrf}:{}), ...(bytes ? {'Content-Type':'application/json','Content-Length':Buffer.byteLength(bytes)}:{}) } }, res => {
      const chunks = []; res.on('data', c=>chunks.push(c)); res.on('end', ()=>resolve({status:res.statusCode, headers:res.headers, text:Buffer.concat(chunks).toString()}));
    }); req.on('error', reject); req.end(bytes);
  });
  const begin = async () => { const response = await request('/auth/discord', {originHeader:null}); assert.equal(response.status,303); const url = new URL(response.headers.location); return {state:url.searchParams.get('state'), challenge:url.searchParams.get('code_challenge'), cookie:response.headers['set-cookie'][0].split(';')[0], response}; };
  const callback = (flow, id=firstId, cookie=flow.cookie) => request(`/auth/discord/callback?code=${id}&state=${flow.state}&iss=${encodeURIComponent('https://discord.com')}`, {cookie, originHeader:null});
  const signIn = async id => { const flow = await begin(), response = await callback(flow,id); assert.equal(response.headers.location,'/'); const cookie = response.headers['set-cookie'].find(c=>c.startsWith('__Host-horizon_session=')).split(';')[0]; const session = JSON.parse((await request('/api/session',{cookie})).text); return {...session,cookie,flow,response}; };
  const profile = (session, name='Builder', skin='alex', extra={}) => request('/api/profile',{method:'POST',cookie:session.cookie,csrf:session.csrf,body:{name,skin},...extra});
  return {root,request,begin,callback,signIn,profile,exchanges,users,restart:async()=>{await gateway.close();await start();}};
}

test('Discord code exchange is browser-bound, single-use and PKCE-protected; no tokens reach the client', async t => {
  const f=await fixture(t), flow=await f.begin();
  assert.match(flow.response.headers['set-cookie'][0],/HttpOnly; SameSite=Lax.*Secure/);
  assert.equal(new URL(flow.response.headers.location).searchParams.get('scope'),'identify');
  const wrong=await f.callback(flow,firstId,'__Host-modcraft_oauth='+'x'.repeat(43));
  assert.equal(wrong.headers.location,'/?auth=failed'); assert.equal(f.exchanges.length,0);
  const response=await f.callback(flow);
  assert.equal(response.headers.location,'/'); assert.equal(f.exchanges.length,1);
  assert.equal(createHash('sha256').update(f.exchanges[0].get('code_verifier')).digest('base64url'),flow.challenge);
  assert.equal((await f.callback(flow)).headers.location,'/?auth=failed'); assert.equal(f.exchanges.length,1);
  assert.doesNotMatch(JSON.stringify(response),new RegExp(clientSecret));
  const stored=await fs.readFile(join(f.root,'private','accounts.json'),'utf8');
  assert.doesNotMatch(stored,/access_token|refresh_token|client_secret/);
  assert.equal((await fs.stat(join(f.root,'private','accounts.json'))).mode & 0o777,0o600);
});

test('first sign-in requires a unique profile; CSRF, reserved names, invalid skins and legacy login are rejected',async t=>{
  const f=await fixture(t), one=await f.signIn(firstId), two=await f.signIn(secondId);
  assert.equal(one.needsProfile,true); assert.equal(one.displayName,'A Friendly Person');
  assert.equal((await f.request('/assets/test.txt',{cookie:one.cookie})).status,403);
  assert.equal((await f.request('/api/login',{method:'POST',body:{invite:'old',name:'Builder'}})).status,410);
  assert.equal((await f.profile(one,'Builder','alex',{csrf:undefined})).status,403);
  assert.equal((await f.profile(one,'Builder','alex',{originHeader:'https://attacker.test'})).status,403);
  assert.equal((await f.profile(one,'Host')).status,403);
  assert.equal((await f.profile(one,'Bad name')).status,400);
  assert.equal((await f.profile(one,'Builder','../../secret')).status,400);
  const race=await Promise.all([f.profile(one,'Builder'),f.profile(two,'builder')]);
  assert.deepEqual(race.map(r=>r.status).sort(),[200,409]);
  const winner=race[0].status===200?one:two, name=race[0].status===200?'Builder':'builder';
  assert.equal((await f.request('/assets/test.txt',{cookie:winner.cookie})).status,200);
  assert.equal((await f.profile(winner,'Rename')).status,409);
  assert.equal((await f.profile(winner,name,'zuri')).status,200);
  const skins=JSON.parse((await f.request('/api/players/skins',{cookie:winner.cookie})).text);
  assert.deepEqual(skins.players[offlineUuid(name)],{name,skin:'zuri'});
  assert.doesNotMatch(JSON.stringify(skins),/discordId|csrf|expires/);
});

test('remembered sessions and profile survive restart, re-login restores them, logout persists',async t=>{
  const f=await fixture(t), one=await f.signIn(firstId);
  assert.match(one.response.headers['set-cookie'].find(c=>c.startsWith('__Host-horizon_session=')),new RegExp(`HttpOnly; SameSite=Strict; Max-Age=${SESSION_SECONDS}; Secure`));
  assert.equal((await f.profile(one,'Returning','kai')).status,200);
  const saved=await fs.readFile(join(f.root,'private','accounts.json'),'utf8'); assert.equal(saved.includes(one.cookie.split('=')[1]),false);
  await f.restart();
  let session=JSON.parse((await f.request('/api/session',{cookie:one.cookie})).text);
  assert.equal(session.name,'Returning'); assert.equal(session.skin,'kai'); assert.equal(session.needsProfile,false);
  const again=await f.signIn(firstId); assert.equal(again.name,'Returning'); assert.equal(again.skin,'kai');
  assert.equal((await f.request('/api/logout',{method:'POST',cookie:one.cookie,csrf:one.csrf})).status,200);
  await f.restart();
  session=JSON.parse((await f.request('/api/session',{cookie:one.cookie})).text); assert.equal(session.authenticated,false);
  assert.equal(JSON.parse((await f.request('/api/session',{cookie:again.cookie})).text).authenticated,true);
});

test('cancellation and provider errors return to the launcher without issuing a session',async t=>{
  const f=await fixture(t), flow=await f.begin();
  const cancelled=await f.request(`/auth/discord/callback?error=access_denied&state=${flow.state}`,{cookie:flow.cookie,originHeader:null});
  assert.equal(cancelled.headers.location,'/?auth=cancelled'); assert.equal(f.exchanges.length,0);
  const broken=await f.callback(await f.begin(),'broken'); assert.equal(broken.headers.location,'/?auth=failed');
  const invalid=await f.begin(); const issuer=await f.request(`/auth/discord/callback?code=foo&state=${invalid.state}&iss=https://evil.test`,{cookie:invalid.cookie});
  assert.equal(issuer.headers.location,'/?auth=failed'); assert.equal(f.exchanges.length,1);
});

test('expired remembered sessions are rejected',async t=>{
  const root=await fs.mkdtemp(join(tmpdir(),'modcraft-expiry-'));t.after(()=>fs.rm(root,{recursive:true,force:true}));
  const accounts=new Accounts(root);await accounts.init();const session=await accounts.signIn({id:firstId});
  const key=Object.keys(accounts.data.sessions)[0];accounts.data.sessions[key].expires=Date.now()-1;
  assert.equal(accounts.get(session.id),undefined);
});
