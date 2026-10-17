import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';

const arg = key => process.argv[process.argv.indexOf(key) + 1];
const input = JSON.parse(await fs.readFile(arg('--input'), 'utf8'));
if (input.hang) await new Promise(() => setInterval(() => {}, 1000));
if (input.reject) { console.log(JSON.stringify({ ok: false })); process.exit(1); }
const js = `export const version = ${JSON.stringify(input.version ?? '1.0.0')};\n`;
if (input.side !== 'server') await fs.writeFile(join(arg('--browser-out'), 'mod.js'), js);
const sha256 = createHash('sha256').update(js).digest('hex');
console.log(JSON.stringify({ ok: true,
  manifest: { id: input.id ?? 'coords_hud', name: 'Coordinates HUD', version: input.version ?? '1.0.0', api: '0.1',
    side: input.side ?? 'client', capabilities: input.capabilities ?? ['game-read', 'hud'], description: 'Fixture', authors: ['Test author'] },
  browser: input.side === 'server' ? null : { entry: 'mod.js', files: [{ path: 'mod.js', sha256: input.badHash ? '0'.repeat(64) : sha256, size: Buffer.byteLength(js) }] },
  checks: { init: 'passed', cloudCredentialInEnvironment: Boolean(process.env.DO_TOKEN) },
}));
