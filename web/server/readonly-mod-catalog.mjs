import * as fs from 'node:fs/promises';
import {join, resolve, sep} from 'node:path';
import {createHash} from 'node:crypto';

// Phase 3 serves only operator-installed, immutable validated artifacts. The
// upload/review/atomic-publication service is introduced separately in Phase 4.
export class ReadonlyModCatalog {
  static async open(directory) {
    const result = new ReadonlyModCatalog(); result.files = new Map();
    const root = await fs.realpath(directory), path = join(root, 'index.json');
    let bytes;
    try { const info = await fs.stat(path); if (info.size > 1024 * 1024) throw new Error('Mod catalog is oversized'); bytes = await fs.readFile(path); }
    catch (error) { if (error.code !== 'ENOENT') throw error; result.index = {schema: 1, revision: 0, mods: []}; return result; }
    const index = JSON.parse(bytes);
    if (index.schema !== 1 || !Number.isSafeInteger(index.revision) || index.revision < 0 || !Array.isArray(index.mods) || index.mods.length > 32 || index.mods.filter(m => m.side !== 'server').length > 8) throw new Error('Invalid installed mod catalog');
    const ids = new Set();
    for (const mod of index.mods) {
      if (!/^[a-z0-9_]{1,64}$/.test(mod.id) || ids.has(mod.id) || !/^[a-f0-9]{64}$/.test(mod.sha256) || mod.manifest?.id !== mod.id || mod.manifest?.side !== mod.side || !['client','server','both'].includes(mod.side)) throw new Error('Invalid installed mod identity');
      ids.add(mod.id);
      if (!Array.isArray(mod.files) || mod.files.length > 64) throw new Error('Invalid installed artifact list');
      let total = 0;
      for (const file of mod.files) {
        if (!/^[A-Za-z0-9_.-]+$/.test(file.path) || file.path.startsWith('.') || !/\.(js|wasm)$/.test(file.path) || !/^[a-f0-9]{64}$/.test(file.sha256) || !Number.isSafeInteger(file.size) || file.size < 0 || file.size > 16 * 1024 * 1024) throw new Error('Invalid installed artifact');
        total += file.size; if (total > 32 * 1024 * 1024) throw new Error('Installed bundle too large');
        const url = `/mods/bundles/${mod.sha256}/${file.path}`;
        if (file.url !== url || result.files.has(url)) throw new Error('Invalid installed artifact URL');
        const relative = `bundles/${mod.sha256}/${file.path}`, real = await fs.realpath(resolve(root, relative));
        if (!real.startsWith(root + sep) || (await fs.lstat(join(root, relative))).isSymbolicLink()) throw new Error('Artifact escapes private storage');
        const content = await fs.readFile(real);
        if (content.length !== file.size || createHash('sha256').update(content).digest('hex') !== file.sha256) throw new Error('Installed artifact checksum mismatch');
        result.files.set(url, relative);
      }
      if (mod.side !== 'server' && !result.files.has(mod.entry)) throw new Error('Missing installed entry point');
    }
    result.index = {schema: 1, revision: index.revision, mods: index.mods}; return result;
  }
}
