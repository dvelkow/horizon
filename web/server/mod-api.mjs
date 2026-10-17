import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';
import { atomicJson, sha256 } from './mod-store.mjs';
import { equalToken } from './security.mjs';

export const MOD_RULES = Object.freeze([
  { id: 'non-game-breaking', text: "Don't break the game.", detail: 'No crashes, world corruption, severe lag, or broken core gameplay.' },
  { id: 'no-free-resources', text: 'No free resources.', detail: 'No granting or duplicating unearned items, materials, currency, or XP.' },
]);
export const SOURCE_BYTES = 512 * 1024;
export const SOURCE_FILES = 64;
export class ModApiError extends Error { constructor(status, message) { super(message); this.status = status; } }
const bad = message => { throw new ModApiError(400, message); };
const scopes = ['mods:submit', 'mods:read-own'];
const publicKey = ({ id, label, created, expires, revoked }) => ({ id, label, created, expires, revoked, scopes });

/** Single gateway writer. Only token digests are persisted, never bearer keys. */
export class ModApiTokens {
  constructor(root) { this.path = join(root, 'mod-api-tokens.json'); this.values = []; this.tail = Promise.resolve(); }
  async init() {
    try {
      const data = JSON.parse(await fs.readFile(this.path, 'utf8'));
      if (data.schema !== 1 || !Array.isArray(data.tokens) || data.tokens.some(t =>
        !/^[a-f0-9]{16}$/.test(t.id) || !/^[a-f0-9]{64}$/.test(t.digest) || !Number.isSafeInteger(t.expires))) throw new Error('Invalid API key store');
      this.values = data.tokens; await fs.chmod(this.path, 0o600);
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  list() { return this.values.map(publicKey); }
  authenticate(header) {
    const match = /^Bearer (mcmod_([a-f0-9]{16})_[A-Za-z0-9_-]{43})$/.exec(header ?? '');
    if (!match) return null;
    const value = this.values.find(t => t.id === match[2]);
    return value && !value.revoked && value.expires > Date.now() && equalToken(sha256(match[1]), value.digest) ? publicKey(value) : null;
  }
  mutate(operation) {
    const result = this.tail.then(async () => {
      const next = structuredClone(this.values), value = operation(next);
      await atomicJson(this.path, { schema: 1, tokens: next }); this.values = next;
      return value;
    });
    this.tail = result.catch(() => {}); return result;
  }
  create({ label, days = 30 }) {
    if (typeof label !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9 _.-]{0,63}$/.test(label)) bad('Use a key label of 1–64 letters, digits, spaces, dots, underscores or hyphens');
    if (!Number.isInteger(days) || days < 1 || days > 90) bad('Key lifetime must be 1–90 days');
    return this.mutate(next => {
      if (next.length >= 256) throw new ModApiError(409, 'API key history is full; contact the host');
      const id = randomBytes(8).toString('hex'), token = `mcmod_${id}_${randomBytes(32).toString('base64url')}`;
      const value = { id, label, created: Date.now(), expires: Date.now() + days * 86400000, revoked: null, digest: sha256(token) };
      next.push(value); return { ...publicKey(value), token };
    });
  }
  revoke(id) {
    return this.mutate(next => {
      const value = next.find(t => t.id === id);
      if (!value) throw new ModApiError(404, 'API key not found');
      value.revoked ??= Date.now(); return publicKey(value);
    });
  }
}

export function parseSubmission(value, maxPackageBytes = 16 * 1024 * 1024) {
  if (typeof value.packageBase64 !== 'string' || !value.packageBase64.length || value.packageBase64.length > Math.ceil(maxPackageBytes / 3) * 4) bad('Supply a nonempty .hmod package as packageBase64 (up to 16 MB)');
  const bytes = Buffer.from(value.packageBase64, 'base64');
  if (bytes.length > maxPackageBytes || bytes.toString('base64') !== value.packageBase64) bad('packageBase64 must be canonical base64 within the package size limit');
  if (typeof value.notes !== 'string' || !value.notes.trim() || value.notes.length > 2000) bad('Include review notes (1–2000 characters) explaining the change and how you tested it');
  if (!Array.isArray(value.files) || !value.files.length || value.files.length > SOURCE_FILES) bad(`Include 1–${SOURCE_FILES} source files as {path, content}`);
  let size = 0;
  const paths = new Set(), files = value.files.map(file => {
    if (!file || typeof file.path !== 'string' || file.path.length > 200 || !/^[A-Za-z0-9_./-]+$/.test(file.path)
        || file.path.split('/').some(part => !part || part.startsWith('.')) || paths.has(file.path)) bad('Source paths must be unique relative paths without hidden files or traversal');
    if (typeof file.content !== 'string' || file.content.includes('\0')) bad('Source file contents must be text');
    size += Buffer.byteLength(file.content);
    if (size > SOURCE_BYTES) bad('Source code exceeds 512 KB');
    paths.add(file.path); return { path: file.path, content: file.content };
  }).sort((a, b) => a.path.localeCompare(b.path));
  const source = { schema: 1, packageSha256: sha256(bytes), notes: value.notes.trim(), files };
  return { bytes, source, sourceSha256: sha256(JSON.stringify(source)), sourceBytes: size };
}

export function submissionView(candidate, index) {
  let status = candidate.status;
  if (index.mods.some(mod => mod.sha256 === candidate.sha256)) status = 'published';
  else if (candidate.status === 'published') status = index.mods.some(mod => mod.id === candidate.manifest?.id) ? 'superseded' : 'revoked';
  const { browser, agentId, ...review } = candidate;
  return { ...review, status, ...(candidate.sourceSha256 ? {sourceUrl: `/api/mods/submissions/${candidate.sha256}/source`} : {}) };
}

/** The source agents read for context: the SDK, its examples and the submission client. */
const SOURCE = 'https://github.com/dvelkow/horizon';
const BRANCH = 'codex/movement-capability';
const tree = path => `${SOURCE}/tree/${BRANCH}${path ? `/${path}` : ''}`;
const blob = path => `${SOURCE}/blob/${BRANCH}/${path}`;

export function apiGuide(origin) {
  return {
    name: '(dev)Modcraft mod submissions', version: 1, rules: MOD_RULES,
    authentication: 'Authorization: Bearer <submission-only key supplied by the host>. Keep it in a private file or environment variable; never in a URL, source bundle or browser.',
    submit: { method: 'POST', url: `${origin}/api/v1/mods/submissions`, contentType: 'application/json',
      body: { packageBase64: '<base64 of the built .hmod file>', notes: '<change, testing, and why it follows the two rules>', files: [{ path: 'src/lib.rs', content: '<source code>' }] } },
    list: { method: 'GET', url: `${origin}/api/v1/mods/submissions` },
    status: { method: 'GET', url: `${origin}/api/v1/mods/submissions/<package-sha256>` },
    limits: { packageBytes: 16 * 1024 * 1024, sourceBytes: SOURCE_BYTES, sourceFiles: SOURCE_FILES, jsonBytes: 24 * 1024 * 1024, submissionsPerMinute: 6 },
    workflow: [
      'Build locally with the Horizon SDK and cargo horizon-mod build. Submit both the built .hmod and its source; the server does not compile or execute uploaded source.',
      'The normal manifest, import/capability, content, size, sandbox-init and browser-bundle checks run on the package. A 202 response means pending review, not approval.',
      'The host reviews the two rules and approves the exact package hash. The API key cannot approve, reject, publish, play, or read another key\'s submissions.',
      'Poll the returned statusUrl. Published code updates reach connected players live. A content mod (new blocks and items) goes live with a world restart; see contentMods. Identical retries are idempotent; changed source/notes for the same package require a new build/version.',
    ],
    contentMods: {
      manifest: 'api = "0.2" and capabilities = ["registries"]; mod.wasm is optional. Blocks and items are declared in data/<id>/horizon/block/*.json and data/<id>/horizon/item/*.json from vanilla templates; assets/ and data/ stay in the mod\'s own namespace, plus additions to vanilla (minecraft) and convention (c) tags. The id cannot be minecraft or c.',
      approval: 'Approving, replacing or revoking a content mod restarts the hosted world: the server tells players, saves, stops and starts again with the new set, and every open page reloads into it; players rejoin by themselves. Its code, if any, runs like any other mod.',
      order: 'The server and every browser layer the same approved content mods, sorted by mod id, so block-state ids match; vanilla ids never move.',
      removal: 'Saves store block and item names. While a content mod is gone its placed blocks load as air and are saved that way, in every chunk loaded meanwhile; approving it again does not bring those back. Items with its ids are unusable until it returns.',
      limits: '64 MiB of content packages per world in all; every player downloads them before joining.',
      example: tree('crates/horizon-mod-sdk/examples/ruby-content'),
    },
    sourceReview: 'Source and notes are immutable review material bound to the package hash. This is not a reproducible-build attestation; source/binary correspondence and gameplay balance need host review.',
    repository: tree(''),
    sdk: blob('docs/mods.md'),
    apiDocs: blob('docs/mod-api.md'),
    client: blob('tools/modcraft_api.py'),
    sdkCrate: tree('crates/horizon-mod-sdk'),
    examples: tree('crates/horizon-mod-sdk/examples'),
    statuses: ['pending', 'rejected', 'published', 'superseded', 'revoked'],
    errors: { 400: 'Invalid payload', 401: 'Missing, invalid, expired or revoked API key', 409: 'Duplicate conflict or storage quota', 413: 'Request too large', 422: 'Package failed validation; see returned error', 429: 'Busy or rate limited; retry after Retry-After seconds' },
  };
}
