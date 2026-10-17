import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

const arg = key => process.argv[process.argv.indexOf(key) + 1];
const input = JSON.parse(await fs.readFile(arg('--input'), 'utf8'));
if (input.hang) await new Promise(() => setInterval(() => {}, 1000));
if (input.reject) { console.log(JSON.stringify({ ok: false })); process.exit(1); }
// A content mod (capability registries) is API 0.2 and may come without code.
const capabilities = input.capabilities ?? ['game-read', 'hud'], content = capabilities.includes('registries');
const code = input.code !== false, browser = input.side !== 'server' && code;
const js = `export const version = ${JSON.stringify(input.version ?? '1.0.0')};\n`;
if (browser) await fs.writeFile(join(arg('--browser-out'), 'mod.js'), js);
const sha256 = createHash('sha256').update(js).digest('hex');
console.log(JSON.stringify({ ok: true,
  manifest: { id: input.id ?? 'coords_hud', name: 'Coordinates HUD', version: input.version ?? '1.0.0', api: content ? '0.2' : '0.1',
    side: input.side ?? 'client', capabilities, description: 'Fixture', authors: ['Test author'] },
  code, ...(content ? { content: { blocks: 2, items: 1, files: 7 } } : {}),
  browser: browser ? { entry: 'mod.js', files: [{ path: 'mod.js', sha256: input.badHash ? '0'.repeat(64) : sha256, size: Buffer.byteLength(js) }] } : null,
  checks: { init: 'passed', cloudCredentialInEnvironment: Boolean(process.env.DO_TOKEN) },
}));
