import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {ModApiTokens, apiGuide, parseSubmission} from '../mod-api.mjs';

test('keys persist as digests, expire, revoke, and serialize concurrent changes',async t=>{
  const root=await fs.mkdtemp(join(tmpdir(),'modcraft-keys-'));t.after(()=>fs.rm(root,{recursive:true,force:true}));
  const store=new ModApiTokens(root); await store.init();
  const [a,b]=await Promise.all([store.create({label:'one',days:1}),store.create({label:'two'})]);
  const restart=new ModApiTokens(root); await restart.init();
  assert.equal(restart.authenticate('Bearer '+a.token).label,'one');
  assert.equal(restart.authenticate('Bearer '+b.token).label,'two');
  assert.equal(restart.authenticate('Bearer '+a.token+'wrong'),null);
  assert.throws(()=>restart.create({label:'bad',days:0}));
  assert.throws(()=>restart.create({label:'bad',days:91}));
  assert.throws(()=>restart.create({label:'bad\nlabel'}));
  await restart.revoke(a.id); assert.equal(restart.authenticate('Bearer '+a.token),null);
  const now=Date.now();t.mock.method(Date,'now',()=>now+91*86400000);
  assert.equal(restart.authenticate('Bearer '+b.token),null);
  assert.equal((await fs.readFile(join(root,'mod-api-tokens.json'),'utf8')).includes(a.token),false);
});

test('source hashing is stable across file order and covers notes and the exact package',()=>{
  const value={packageBase64:Buffer.from('package').toString('base64'),notes:'notes',files:[{path:'src/b.rs',content:'b'},{path:'src/a.rs',content:'a'}]};
  const a=parseSubmission(value),b=parseSubmission({...value,files:[...value.files].reverse()});
  assert.equal(a.sourceSha256,b.sourceSha256);
  assert.notEqual(a.sourceSha256,parseSubmission({...value,notes:'other'}).sourceSha256);
  for(const files of [[{path:'.env',content:'secret'}],[{path:'src//a',content:'x'}],[value.files[0],value.files[0]],Array.from({length:65},(_,i)=>({path:`${i}.rs`,content:'x'}))]) assert.throws(()=>parseSubmission({...value,files}));
});

test('the guide points agents at this branch and says content mods restart the world',()=>{
  const guide=apiGuide('https://game.example.test'),branch='https://github.com/dvelkow/horizon';
  assert.equal(guide.repository,`${branch}/tree/codex/movement-capability`);
  for(const [key,path] of [['sdk','blob/codex/movement-capability/docs/mods.md'],['apiDocs','blob/codex/movement-capability/docs/mod-api.md'],
    ['client','blob/codex/movement-capability/tools/modcraft_api.py'],['sdkCrate','tree/codex/movement-capability/crates/horizon-mod-sdk'],
    ['examples','tree/codex/movement-capability/crates/horizon-mod-sdk/examples']]) assert.equal(guide[key],`${branch}/${path}`,key);
  assert.equal(JSON.stringify(guide).includes('agent-mod-submissions'),false);
  assert.match(guide.contentMods.approval,/restarts the hosted world/);
  assert.match(guide.contentMods.order,/sorted by mod id/);
  assert.doesNotMatch(JSON.stringify(guide),/refuse|cannot be approved/i);
});
