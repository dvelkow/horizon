import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {PNG} from 'pngjs';
import {MinecraftSkins, normalizeSkin} from '../minecraft-skins.mjs';
import {Accounts} from '../accounts.mjs';

const id='a'.repeat(32), texture='b'.repeat(64);
const png=(height=64,red=120)=>PNG.sync.write({width:64,height,data:Buffer.from(Array(64*height).fill([red,60,20,255]).flat())});
async function fixture(t) {
  const root=await fs.mkdtemp(join(tmpdir(),'modcraft-skins-'));t.after(()=>fs.rm(root,{recursive:true,force:true}));
  const state={url:`http://textures.minecraft.net/texture/${texture}`,model:'slim',image:png(),calls:[],missing:false};
  const fetcher=async(url,options)=>{
    state.calls.push(url); assert.equal(options.redirect,'error');
    if(url.startsWith('https://api.mojang.com/')) return state.missing ? new Response(null,{status:404}) : Response.json({id,name:'Builder'});
    if(url.startsWith('https://sessionserver.mojang.com/')) return Response.json({id,properties:[{name:'textures',value:Buffer.from(JSON.stringify({profileId:id,textures:{SKIN:{url:state.url,metadata:{model:state.model}}}})).toString('base64')}]});
    assert.equal(url,`https://textures.minecraft.net/texture/${texture}`);
    return new Response(state.image);
  };
  const store=new MinecraftSkins(root,fetcher);await store.init();return{root,state,fetcher,store};
}
test('live username link persists, shares a lookup, refreshes texture without changing player identity, and survives provider outages',async t=>{
  const {root,state,fetcher,store}=await fixture(t);
  const [a,b]=await Promise.all([store.copy('Builder'),store.copy('builder')]);
  assert.equal(a.id,'minecraft:Builder');assert.deepEqual(a,b);assert.equal(state.calls.length,3);assert.equal(a.model,'slim');
  const accounts=new Accounts(root,{skinStore:store});await accounts.init();const session=await accounts.signIn({id:'123456789012345678'});
  const saved=await accounts.saveProfile(session,{name:'OurName',skin:a.id}); assert.equal(saved.name,'OurName');
  const first=accounts.skins().players[saved.uuid].skin;assert.equal(first,a.textureId);
  store.cache.get('builder').expires=0;state.image=png(64,200);state.model='wide';
  assert.equal(store.latest('Builder',a).textureId,first);await store.pending.get('builder');
  const refreshed=accounts.skins().players[saved.uuid].skin;assert.notEqual(refreshed,first);assert.match(refreshed,/^mc_wide_/);
  assert.equal(accounts.get(session.id).name,'OurName');
  const restored=new MinecraftSkins(root,fetcher);await restored.init();assert.equal(restored.linked(a.id).textureId,refreshed);
  const reopened=new Accounts(root,{skinStore:restored});await reopened.init();assert.equal(reopened.skins().players[saved.uuid].skin,refreshed);
  restored.cache.get('builder').expires=0;state.missing=true;
  assert.equal(restored.latest('Builder',a).textureId,refreshed);await restored.pending.get('builder').catch(()=>{});
  assert.equal(restored.latest('Builder',a).textureId,refreshed);
  assert.equal((await fs.stat(join(root,'player-skins',refreshed+'.png'))).mode&0o777,0o600);
});
test('rejects malformed names, missing players and texture URLs that could access private hosts',async t=>{
  const {state,store}=await fixture(t);
  await assert.rejects(store.copy('../secret'),e=>e.status===400);assert.equal(state.calls.length,0);
  state.missing=true;await assert.rejects(store.copy('Builder'),e=>e.status===404);state.missing=false;
  for(const url of ['http://127.0.0.1/private','https://textures.minecraft.net.evil.test/texture/'+texture,'https://textures.minecraft.net/texture/'+texture+'?redirect=evil']) {
    state.url=url;const before=state.calls.length;await assert.rejects(store.copy('Builder'),e=>e.status===422);assert.equal(state.calls.length-before,2);
  }
});
test('normalizes legacy PNGs and alpha, rejects invalid, corrupt and oversized images',()=>{
  const legacy=PNG.sync.read(normalizeSkin(png(32)));assert.equal(legacy.height,64);
  const at=(x,y)=>(y*64+x)*4;
  assert.equal(legacy.data[at(40,8)+3],0);assert.equal(legacy.data[at(20,52)+3],255);assert.equal(legacy.data[at(36,52)],120);
  const corrupt=Buffer.from(png());corrupt[50]^=255;
  for(const bytes of [Buffer.from('not png'),corrupt,Buffer.alloc(65537),PNG.sync.write({width:128,height:64,data:Buffer.alloc(128*64*4)})]) assert.throws(()=>normalizeSkin(bytes),e=>e.status===422);
});
