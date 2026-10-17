// Precompressed copies of the browser release's compressible files (`.br`,
// `.gz`) beside them: the gateway serves one when the browser accepts it, so
// the 31 MB WASM crosses the network as ~5 MB. Brotli at its best setting
// takes about a minute for the WASM, so this runs once per build, never per
// request. Content-hashed bundles keep their copies between builds.
// Usage: node web/precompress.mjs .local/web
import { brotliCompress, gzip, constants } from 'node:zlib';
import { promisify } from 'node:util';
import * as fs from 'node:fs/promises';
import { join } from 'node:path';

const br = promisify(brotliCompress), gz = promisify(gzip);
const root = process.argv[2];
if (!root) throw new Error('usage: node web/precompress.mjs PUBLIC_DIR');

const top = ['index.html', 'app.js', 'bootstrap.js', 'rust-worker.js', 'wardrobe.js', 'style.css', 'portal.html', 'portal.js'];
const files = [...top];
for (const dir of ['pkg', 'mods-runtime']) {
  for (const name of await fs.readdir(join(root, dir)).catch(() => [])) {
    if (/\.(js|wasm)$/.test(name)) files.push(`${dir}/${name}`);
  }
}
for (const name of await fs.readdir(join(root, 'assets')).catch(() => [])) {
  if (/^[a-f0-9]{64}\.(json|zip)$/.test(name)) files.push(`assets/${name}`);
}

const stale = async (source, copy) => {
  try { return (await fs.stat(copy)).mtimeMs < (await fs.stat(source)).mtimeMs; } catch { return true; }
};
let saved = 0;
await Promise.all(files.map(async name => {
  const source = join(root, name);
  let bytes;
  try { bytes = await fs.readFile(source); } catch { return; }
  for (const [ext, compress] of [
    ['.br', b => br(b, { params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_LGWIN]: 24,
      [constants.BROTLI_PARAM_SIZE_HINT]: b.length } })],
    ['.gz', b => gz(b, { level: 9 })],
  ]) {
    const copy = source + ext;
    if (!(await stale(source, copy))) continue;
    const packed = await compress(bytes);
    // Not worth a variant unless it saves a tenth.
    if (packed.length > bytes.length * 0.9) { await fs.rm(copy, { force: true }); continue; }
    await fs.writeFile(copy, packed);
    if (ext === '.br') saved += bytes.length - packed.length;
  }
}));
console.log(`Precompressed ${files.length} files (Brotli saves ${(saved / 1e6).toFixed(1)} MB on the rebuilt ones)`);
