import * as fs from 'node:fs/promises';
import { resolve, sep } from 'node:path';
import { createGateway } from './gateway.mjs';

const path = process.argv[2] ?? process.env.HORIZON_GATEWAY_CONFIG;
if (!path) throw new Error('Pass the private gateway configuration file path');
const stat = await fs.stat(path);
if (stat.mode & 0o077) throw new Error('Gateway configuration must be readable only by its owner (mode 0600)');
const config = JSON.parse(await fs.readFile(resolve(path), 'utf8'));
const realConfigPath = await fs.realpath(path);
for (const directory of [config.publicDir, config.assetsDir, config.modsRuntimeDir].filter(Boolean)) {
  const root = await fs.realpath(directory);
  if (realConfigPath.startsWith(`${root}${sep}`)) throw new Error('Gateway configuration cannot live inside a served directory');
}
const gateway = await createGateway(config);
const address = config.listenHost ?? '127.0.0.1', port = config.listenPort ?? 8080;
if (!['127.0.0.1', '::1'].includes(address)) throw new Error('Bind the gateway to loopback behind the TLS proxy');
gateway.server.listen(port, address, () => console.log(`Horizon gateway listening on loopback port ${port}`));
let stopping = false;
const stop = async () => { if (stopping) return; stopping = true; await gateway.close(); };
process.once('SIGTERM', stop); process.once('SIGINT', stop);
