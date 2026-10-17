import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { dirname, join, resolve } from 'node:path';
import { runValidator } from './validator.mjs';

export const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
export const isHash = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const capabilities = new Set(['game-read', 'chat', 'hud', 'movement', 'registries']);
/** Every browser downloads each content mod's package before it joins (web/mods/host.js). */
export const MAX_CONTENT_BYTES = 64 * 1024 * 1024;
/** Content mods add blocks and items: they load when the world starts, never live. */
export const isContent = manifest => manifest?.capabilities?.includes('registries') === true;

export async function atomicJson(path, value) {
  const temporary = `${path}.${randomBytes(12).toString('hex')}.tmp`;
  const handle = await fs.open(temporary, 'wx', 0o600);
  try { await handle.writeFile(`${JSON.stringify(value, null, 2)}\n`); await handle.sync(); }
  finally { await handle.close(); }
  await fs.rename(temporary, path);
  const directory = await fs.open(dirname(path), 'r');
  try { await directory.sync(); } finally { await directory.close(); }
}

function manifestSummary(manifest) {
  if (!manifest || !/^[a-z0-9_]{1,64}$/.test(manifest.id ?? '') ||
      typeof manifest.name !== 'string' || manifest.name.length > 128 || !manifest.name ||
      typeof manifest.version !== 'string' || manifest.version.length > 64 || !manifest.version ||
      !['0.1', '0.2'].includes(manifest.api) || !['client', 'server', 'both'].includes(manifest.side) ||
      !Array.isArray(manifest.capabilities) || manifest.capabilities.length > capabilities.size ||
      manifest.capabilities.some(value => !capabilities.has(value))) throw new Error('Validator returned an invalid manifest');
  if (manifest.description != null && (typeof manifest.description !== 'string' || manifest.description.length > 4096)) throw new Error('Invalid mod description');
  if (manifest.authors != null && (!Array.isArray(manifest.authors) || manifest.authors.length > 32 || manifest.authors.some(name => typeof name !== 'string' || name.length > 256))) throw new Error('Invalid mod authors');
  return { id: manifest.id, name: manifest.name, version: manifest.version, api: manifest.api,
    side: manifest.side, capabilities: [...new Set(manifest.capabilities)],
    description: manifest.description ?? '', authors: manifest.authors ?? [] };
}

function contentSummary(content) {
  const count = value => Number.isSafeInteger(value) && value >= 0 ? value : 0;
  return { blocks: count(content?.blocks), items: count(content?.items), files: count(content?.files) };
}

function plainBundlePath(path) {
  return typeof path === 'string' && path.length <= 200 && /^[A-Za-z0-9_./-]+$/.test(path) &&
    path.split('/').every(part => part && part !== '.' && part !== '..' && !part.startsWith('.')) &&
    /\.(js|mjs|wasm|json|ts)$/.test(path);
}

async function bundleFiles(root) {
  const found = [];
  async function visit(directory, prefix = '') {
    for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
      const name = `${prefix}${entry.name}`;
      if (entry.isSymbolicLink()) throw new Error('Generated bundles cannot contain symlinks');
      if (entry.isDirectory()) {
        if (!/^[A-Za-z0-9_-]+$/.test(entry.name) || prefix.split('/').length > 6) throw new Error('Invalid bundle directory');
        await visit(join(directory, entry.name), `${name}/`);
      } else if (entry.isFile() && plainBundlePath(name)) {
        found.push(name);
        if (found.length > 64) throw new Error('Too many generated bundle files');
      } else throw new Error('Unexpected generated bundle file');
    }
  }
  await visit(root);
  return found.sort();
}

async function verifiedBundle(root, browser) {
  if (!browser || !plainBundlePath(browser.entry) || !/\.(m?js)$/.test(browser.entry) || !Array.isArray(browser.files)) throw new Error('Validator did not produce a browser bundle');
  const actual = await bundleFiles(root);
  const expected = browser.files.map(file => file.path).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected) || !actual.includes(browser.entry)) throw new Error('Browser bundle file list does not match the validator result');
  let total = 0;
  const files = [];
  for (const path of actual) {
    const info = await fs.lstat(join(root, path));
    total += info.size;
    if (info.size > 16 * 1024 * 1024 || total > 32 * 1024 * 1024) throw new Error('Browser bundle exceeds its size limit');
    const bytes = await fs.readFile(join(root, path));
    const hash = sha256(bytes), reported = browser.files.find(file => file.path === path);
    if (!isHash(reported.sha256) || reported.sha256 !== hash || reported.size !== bytes.length) throw new Error('Browser bundle hash does not match its validation report');
    files.push({ path, sha256: hash, size: bytes.length });
  }
  return { entry: browser.entry, files };
}

async function immutableCopy(source, destination, expectedHash) {
  await fs.mkdir(dirname(destination), { recursive: true, mode: 0o700 });
  try { await fs.copyFile(source, destination, constants.COPYFILE_EXCL); }
  catch (error) { if (error.code !== 'EEXIST') throw error; }
  if (sha256(await fs.readFile(destination)) !== expectedHash) throw new Error('Immutable artifact already exists with different bytes');
  await fs.chmod(destination, 0o400);
  const handle = await fs.open(destination, 'r');
  try { await handle.sync(); } finally { await handle.close(); }
}

export class ModStore {
  constructor(config) { this.config = config; this.root = resolve(config.privateDir); this.tail = Promise.resolve(); }
  async init() {
    for (const directory of ['', 'candidates', 'packages', 'bundles', 'approved-server']) await fs.mkdir(join(this.root, directory), { recursive: true, mode: 0o700 });
    await fs.chmod(this.root, 0o700);
    this.indexPath = join(this.root, 'index.json');
    try { this.index = JSON.parse(await fs.readFile(this.indexPath, 'utf8')); }
    catch (error) {
      if (error.code !== 'ENOENT') throw new Error('Published mod index is unreadable');
      this.index = { schema: 1, revision: 0, mods: [], releasedHashes: [] }; await atomicJson(this.indexPath, this.index);
    }
    if (this.index.schema !== 1 || !Number.isSafeInteger(this.index.revision) || !Array.isArray(this.index.mods)) throw new Error('Published mod index is invalid');
    this.index.releasedHashes ??= this.index.mods.map(mod => mod.sha256);
    if (!Array.isArray(this.index.releasedHashes) || this.index.releasedHashes.some(hash => !isHash(hash))) throw new Error('Published mod history is invalid');
    await this.reconcileServerMods();
  }
  /** The durable index is authoritative; reconcile again after an interrupted publication. */
  async reconcileServerMods() {
    const root = join(this.root, 'approved-server');
    const wanted = new Set();
    for (const mod of this.index.mods) {
      // The server loads every content mod's blocks and items, a players' one too.
      if (!['server', 'both'].includes(mod.side) && !isContent(mod.manifest)) continue;
      manifestSummary(mod.manifest);
      if (!isHash(mod.sha256)) throw new Error('Invalid approved server package hash');
      const name = `${mod.id}.hmod`; wanted.add(name);
      const destination = join(root, name);
      try { if (sha256(await fs.readFile(destination)) === mod.sha256) continue; }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      const source = join(this.root, 'packages', `${mod.sha256}.hmod`);
      if (sha256(await fs.readFile(source)) !== mod.sha256) throw new Error('Approved server package failed its integrity check');
      const temporary = join(root, `.${mod.id}.${randomBytes(12).toString('hex')}.tmp`);
      await immutableCopy(source, temporary, mod.sha256);
      await fs.rename(temporary, destination);
    }
    for (const name of await fs.readdir(root)) {
      if ((!wanted.has(name) && /^[a-z0-9_]{1,64}\.hmod$/.test(name)) || /^\.[a-z0-9_]+\.[a-f0-9]{24}\.tmp$/.test(name)) await fs.unlink(join(root, name));
    }
    const directory = await fs.open(root, 'r');
    try { await directory.sync(); } finally { await directory.close(); }
  }
  /** The active content mods, by id: the world's blocks and items. The gateway's release names
   * them, so open pages reload when they change and the server restarts to load them. */
  contentSet() {
    return this.index.mods.filter(mod => isContent(mod.manifest)).map(mod => ({ id: mod.id, sha256: mod.sha256 }))
      .sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  }
  candidatePath(hash) { if (!isHash(hash)) throw new Error('Invalid candidate hash'); return join(this.root, 'candidates', hash); }
  async readCandidate(hash) { return JSON.parse(await fs.readFile(join(this.candidatePath(hash), 'candidate.json'), 'utf8')); }
  async history() {
    const entries = await fs.readdir(join(this.root, 'candidates'));
    const candidates = [];
    for (const hash of entries.filter(isHash)) {
      try { candidates.push(await this.readCandidate(hash)); }
      catch { /* Interrupted uploads have no reviewable candidate. */ }
    }
    return candidates.sort((a, b) => a.created.localeCompare(b.created));
  }
  async pending() { return (await this.history()).filter(candidate => candidate.status === 'pending'); }
  async readSource(hash) {
    const candidate = await this.readCandidate(hash);
    if (!candidate.sourceSha256) return null;
    const source = JSON.parse(await fs.readFile(join(this.candidatePath(hash), 'source.json'), 'utf8'));
    if (source.packageSha256 !== hash || sha256(JSON.stringify(source)) !== candidate.sourceSha256) throw new Error('Review source failed its integrity check');
    return source;
  }
  async upload(bytes, uploader, review = null) {
    const hash = sha256(bytes), directory = this.candidatePath(hash);
    try { return await this.readCandidate(hash); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    if ((await this.pending()).length >= (this.config.maxCandidates ?? 32)) throw new Error('Candidate storage is full; ask the owner to review pending uploads');
    // A process can stop before writing candidate.json. Only this exact digest's
    // incomplete staging directory is cleared, never released artifacts.
    await fs.rm(directory, { recursive: true, force: true });
    await fs.mkdir(directory, { mode: 0o700 });
    const input = join(directory, 'package.hmod'), browserRoot = join(directory, 'browser');
    await fs.writeFile(input, bytes, { flag: 'wx', mode: 0o400 });
    await fs.mkdir(browserRoot, { mode: 0o700 });
    let candidate;
    try {
      const result = await runValidator(this.config, input, browserRoot);
      if (sha256(await fs.readFile(input)) !== hash) throw new Error('Uploaded package changed during validation');
      const manifest = manifestSummary(result.manifest), content = isContent(manifest);
      // Only a content mod may come without code (horizon-mods Package::read).
      const code = result.code !== false;
      if (!code && !content) throw new Error('Validator reported a mod without code or content');
      const browser = manifest.side === 'server' || !code ? null : await verifiedBundle(browserRoot, result.browser);
      candidate = { schema: 1, sha256: hash, status: 'pending', uploader, created: new Date().toISOString(), manifest, browser,
        checks: result.checks ?? {}, size: bytes.length, code,
        ...(content ? { content: contentSummary(result.content) } : {}),
        // Code goes live; blocks and items restart the world to load them.
        liveReload: !content, restartsWorld: content };
    } catch (error) {
      candidate = { schema: 1, sha256: hash, status: 'rejected', uploader, created: new Date().toISOString(), error: error.message };
    }
    if (review) {
      candidate = { ...candidate, submittedVia: 'api', agentId: review.agentId, notes: review.source.notes,
        sourceSha256: review.sourceSha256, sourceBytes: review.sourceBytes, sourceFiles: review.source.files.length };
      await atomicJson(join(directory, 'source.json'), review.source);
    }
    await atomicJson(join(directory, 'candidate.json'), candidate);
    if (candidate.status === 'rejected') await this.clearCandidatePayload(hash);
    return candidate;
  }
  async clearCandidatePayload(hash) {
    const directory = this.candidatePath(hash);
    await fs.rm(join(directory, 'browser'), { recursive: true, force: true });
    await fs.rm(join(directory, 'package.hmod'), { force: true });
  }
  approve(hash) {
    const operation = this.tail.then(async () => {
      const candidate = await this.readCandidate(hash);
      if (candidate.status === 'published' || this.index.mods.some(mod => mod.sha256 === hash)) {
        await this.reconcileServerMods();
        if (candidate.status !== 'published') {
          candidate.status = 'published'; candidate.publishedRevision = this.index.revision;
          await atomicJson(join(this.candidatePath(hash), 'candidate.json'), candidate);
        }
        await this.clearCandidatePayload(hash);
        return this.index;
      }
      if (candidate.status !== 'pending') throw new Error('Only a pending, validated candidate can be approved');
      if (candidate.sourceSha256) await this.readSource(hash);
      // The browser host rejects indexes larger than 32. Keep publication
      // within that contract; replacing an existing ID does not use a slot.
      const activeLimit = Math.min(this.config.maxActiveMods ?? 32, 32);
      if (!this.index.mods.some(mod => mod.id === candidate.manifest.id) && this.index.mods.length >= activeLimit) throw new Error('Active mod limit reached; revoke a mod before approving another ID');
      // Browser Workers run client code; content-only and server mods use none.
      const needsBundle = candidate.manifest.side !== 'server' && candidate.code !== false;
      const browserMods = this.index.mods.filter(mod => mod.id !== candidate.manifest.id && mod.side !== 'server' && mod.entry).length;
      if (needsBundle && browserMods >= 8) throw new Error('Browser mod limit reached (8); revoke a browser mod before approving another ID');
      if (isContent(candidate.manifest)) {
        const others = this.index.mods.filter(mod => mod.id !== candidate.manifest.id && isContent(mod.manifest)).reduce((sum, mod) => sum + mod.size, 0);
        if (others + candidate.size > (this.config.maxContentBytes ?? MAX_CONTENT_BYTES)) throw new Error('Content mod limit reached: every player downloads them all before joining; revoke a content mod first');
      }
      if (!this.index.releasedHashes.includes(hash) && this.index.releasedHashes.length >= (this.config.maxPublishedVersions ?? 128)) throw new Error('Published version limit reached; archive old releases during maintenance');
      const directory = this.candidatePath(hash), input = join(directory, 'package.hmod');
      if (sha256(await fs.readFile(input)) !== hash) throw new Error('Candidate package changed after review');
      const bundle = needsBundle ? await verifiedBundle(join(directory, 'browser'), candidate.browser) : null;
      await immutableCopy(input, join(this.root, 'packages', `${hash}.hmod`), hash);
      for (const file of bundle?.files ?? []) await immutableCopy(join(directory, 'browser', file.path), join(this.root, 'bundles', hash, file.path), file.sha256);
      const mod = { ...candidate.manifest, manifest: candidate.manifest, sha256: hash, size: candidate.size,
        url: `/mods/packages/${hash}.hmod`, entry: bundle ? `/mods/bundles/${hash}/${bundle.entry}` : null,
        files: (bundle?.files ?? []).map(file => ({ ...file, url: `/mods/bundles/${hash}/${file.path}` })) };
      const index = { schema: 1, revision: this.index.revision + 1, mods: [...this.index.mods.filter(value => value.id !== mod.id), mod].sort((a, b) => a.id.localeCompare(b.id)), releasedHashes: [...new Set([...this.index.releasedHashes, hash])] };
      await atomicJson(this.indexPath, index);
      this.index = index;
      await this.reconcileServerMods();
      candidate.status = 'published'; candidate.publishedRevision = index.revision;
      // The durable index is authoritative if a crash precedes this review annotation.
      await atomicJson(join(directory, 'candidate.json'), candidate);
      await this.clearCandidatePayload(hash);
      return index;
    });
    this.tail = operation.catch(() => {});
    return operation;
  }
  reject(hash) {
    const operation = this.tail.then(async () => {
      const candidate = await this.readCandidate(hash);
      if (candidate.status !== 'pending') throw new Error('Candidate is not pending');
      candidate.status = 'rejected'; candidate.error = 'Rejected by the owner';
      await atomicJson(join(this.candidatePath(hash), 'candidate.json'), candidate);
      await this.clearCandidatePayload(hash);
    });
    this.tail = operation.catch(() => {});
    return operation;
  }
  revoke(hash) {
    const operation = this.tail.then(async () => {
      if (!this.index.mods.some(mod => mod.sha256 === hash)) throw new Error('That hash is not the active approved version');
      const index = { ...this.index, revision: this.index.revision + 1, mods: this.index.mods.filter(mod => mod.sha256 !== hash) };
      await atomicJson(this.indexPath, index); this.index = index;
      await this.reconcileServerMods();
      return index;
    });
    this.tail = operation.catch(() => {});
    return operation;
  }
  publishedPath(path) {
    const packageMatch = /^\/mods\/packages\/([a-f0-9]{64})\.hmod$/.exec(path);
    if (packageMatch && this.index.releasedHashes.includes(packageMatch[1])) return join(this.root, 'packages', `${packageMatch[1]}.hmod`);
    const match = /^\/mods\/bundles\/([a-f0-9]{64})\/(.+)$/.exec(path);
    if (!match || !this.index.releasedHashes.includes(match[1]) || !plainBundlePath(match[2])) return null;
    // Older immutable versions remain fetchable by already-connected clients.
    return join(this.root, 'bundles', match[1], match[2]);
  }
}
