import * as fs from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { atomicJson } from './mod-store.mjs';
import { randomToken } from './security.mjs';

export const SKINS = Object.freeze(['steve', 'alex', 'ari', 'efe', 'kai', 'makena', 'noor', 'sunny', 'zuri']);
export const SESSION_SECONDS = 30 * 24 * 60 * 60;
const digest = value => createHash('sha256').update(value).digest('hex');
export class ProfileError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}
export function offlineUuid(name) {
  const value = createHash('md5').update(`OfflinePlayer:${name}`).digest();
  value[6] = (value[6] & 15) | 48; value[8] = (value[8] & 63) | 128;
  return value.toString('hex');
}

/** One gateway owns this atomic private store. Discord tokens are never saved. */
export class Accounts {
  constructor(root, { protectedNames = [], ownerIds = [] } = {}) {
    this.file = join(root, 'accounts.json'); this.tail = Promise.resolve();
    this.protectedNames = new Set(protectedNames.map(name => name.toLowerCase()));
    this.ownerIds = new Set(ownerIds);
  }
  async init() {
    try { this.data = JSON.parse(await fs.readFile(this.file, 'utf8')); }
    catch (error) {
      if (error.code !== 'ENOENT') throw new Error('Account store is unreadable');
      this.data = { schema: 1, revision: 0, users: {}, sessions: {} };
      await atomicJson(this.file, this.data);
    }
    if (this.data.schema !== 1 || !this.data.users || !this.data.sessions) throw new Error('Account store is invalid');
    await fs.chmod(this.file, 0o600);
  }
  mutate(action) {
    const task = this.tail.then(async () => {
      const next = structuredClone(this.data);
      for (const [key, session] of Object.entries(next.sessions)) if (session.expires <= Date.now()) delete next.sessions[key];
      const result = action(next);
      await atomicJson(this.file, next); this.data = next;
      return result;
    });
    this.tail = task.catch(() => {}); return task;
  }
  async signIn(identity, previousToken) {
    if (!/^[0-9]{17,20}$/.test(identity.id ?? '') || identity.bot) throw new ProfileError(403, 'A personal Discord account is required');
    const token = randomToken(), csrf = randomToken(), expires = Date.now() + SESSION_SECONDS * 1000;
    await this.mutate(data => {
      if (!data.users[identity.id] && Object.keys(data.users).length >= 1024) throw new ProfileError(503, 'The world has reached its account limit');
      const existing = data.users[identity.id] ?? { discordId: identity.id, created: Date.now() };
      data.users[identity.id] = { ...existing, displayName: String(identity.global_name ?? identity.username ?? 'Player').slice(0, 80) };
      if (previousToken) delete data.sessions[digest(previousToken)];
      const own = Object.entries(data.sessions).filter(([, value]) => value.discordId === identity.id).sort((a, b) => a[1].expires - b[1].expires);
      for (const [key] of own.slice(0, Math.max(0, own.length - 7))) delete data.sessions[key];
      if (Object.keys(data.sessions).length >= 4096) throw new ProfileError(503, 'Too many remembered sessions; try again later');
      data.sessions[digest(token)] = { discordId: identity.id, csrf, expires };
    });
    return this.get(token);
  }
  get(token) {
    if (typeof token !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(token)) return undefined;
    const session = this.data.sessions[digest(token)];
    if (!session || session.expires <= Date.now()) return undefined;
    const user = this.data.users[session.discordId];
    if (!user) return undefined;
    return { ...user, ...session, id: token };
  }
  delete(token) { return this.mutate(data => { if (token) delete data.sessions[digest(token)]; }); }
  async saveProfile(session, { name, skin }, owner = false) {
    if (typeof name !== 'string' || !/^[A-Za-z0-9_]{1,16}$/.test(name)) throw new ProfileError(400, 'Use 1–16 letters, numbers, or underscores for your in-game name');
    if (!SKINS.includes(skin)) throw new ProfileError(400, 'Choose a skin from the wardrobe');
    await this.mutate(data => {
      const user = data.users[session.discordId];
      if (!user) throw new ProfileError(401, 'Sign in with Discord again');
      // Offline world saves use this exact name to derive their identity.
      if (user.name && user.name !== name) throw new ProfileError(409, 'Your in-game name is already linked to your world progress');
      if (!user.name && this.protectedNames.has(name.toLowerCase()) && !owner && !this.ownerIds.has(session.discordId)) throw new ProfileError(403, 'That name is reserved for the host');
      if (Object.values(data.users).some(other => other.discordId !== user.discordId && other.name?.toLowerCase() === name.toLowerCase())) throw new ProfileError(409, 'That name is already taken. Try another');
      user.name = name; user.skin = skin; user.uuid = offlineUuid(name); data.revision++;
    });
    return this.get(session.id);
  }
  skins() {
    return { revision: this.data.revision, players: Object.fromEntries(Object.values(this.data.users).filter(user => user.name).map(user => [user.uuid, { name: user.name, skin: user.skin }])) };
  }
}
