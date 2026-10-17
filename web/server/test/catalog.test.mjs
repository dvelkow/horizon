import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
import {ReadonlyModCatalog} from '../readonly-mod-catalog.mjs';
test('operator catalog serves only listed artifacts with matching hashes', async t => {
 const root = await fs.mkdtemp(join(tmpdir(),'horizon-catalog-')); t.after(() => fs.rm(root,{recursive:true,force:true}));
 const hash = 'a'.repeat(64), bytes = Buffer.from('export const example = 1;'), sha256 = createHash('sha256').update(bytes).digest('hex');
 await fs.mkdir(join(root,'bundles',hash),{recursive:true}); await fs.writeFile(join(root,'bundles',hash,'mod.js'),bytes);
 const file = {path:'mod.js',sha256,size:bytes.length,url:`/mods/bundles/${hash}/mod.js`};
 const entry = {id:'example',sha256:hash,side:'client',manifest:{id:'example',side:'client'},entry:file.url,files:[file]};
 await fs.writeFile(join(root,'index.json'),JSON.stringify({schema:1,revision:1,mods:[entry]}));
 const catalog = await ReadonlyModCatalog.open(root); assert.equal(catalog.files.size,1); assert.equal(catalog.files.get(file.url),`bundles/${hash}/mod.js`);
 await fs.writeFile(join(root,'bundles',hash,'mod.js'),'changed'); await assert.rejects(ReadonlyModCatalog.open(root),/checksum/);
});
