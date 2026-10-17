import { randomBytes, scrypt as derive, timingSafeEqual } from 'node:crypto';
import { promisify } from 'node:util';

const scrypt = promisify(derive);
const COST = { N: 16384, r: 8, p: 1, maxmem: 64 * 1024 * 1024 };
export const randomToken = () => randomBytes(32).toString('base64url');

export async function hashSecret(secret) {
  if (typeof secret !== 'string' || secret.length < 16 || secret.length > 1024) throw new Error('Secrets must contain 16–1024 characters');
  const salt = randomBytes(16);
  const key = await scrypt(secret, salt, 32, COST);
  return `scrypt$16384$8$1$${salt.toString('hex')}$${key.toString('hex')}`;
}

export function validSecretHash(hash) {
  return typeof hash === 'string' && /^scrypt\$16384\$8\$1\$[a-f0-9]{32}\$[a-f0-9]{64}$/.test(hash);
}

export async function verifySecret(secret, encoded) {
  if (!validSecretHash(encoded) || typeof secret !== 'string' || secret.length > 1024) return false;
  const [, , , , salt, expected] = encoded.split('$');
  const key = await scrypt(secret, Buffer.from(salt, 'hex'), 32, COST);
  return timingSafeEqual(key, Buffer.from(expected, 'hex'));
}

export function equalToken(actual, expected) {
  if (typeof actual !== 'string' || typeof expected !== 'string') return false;
  const a = Buffer.from(actual), b = Buffer.from(expected);
  return a.length === b.length && timingSafeEqual(a, b);
}

export function cookieValue(request, name) {
  const all = (request.headers.cookie ?? '').split(';').map(part => part.trim());
  const matches = all.filter(part => part.startsWith(`${name}=`));
  if (matches.length !== 1) return undefined;
  const value = matches[0].slice(name.length + 1);
  return /^[A-Za-z0-9_-]{43}$/.test(value) ? value : undefined;
}

export function sessionCookie(name, token, seconds, secure) {
  return `${name}=${token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${seconds}${secure ? '; Secure' : ''}`;
}

export class Sessions {
  constructor({ capacity = 1024, lifetimeMs = 8 * 60 * 60 * 1000 } = {}) {
    this.values = new Map(); this.capacity = capacity; this.lifetimeMs = lifetimeMs;
  }
  prune() {
    for (const [id, session] of this.values) if (session.expires <= Date.now()) this.values.delete(id);
  }
  create(data) {
    this.prune();
    if (this.values.size >= this.capacity) throw new Error('Session capacity reached');
    const id = randomToken(), session = { ...data, id, csrf: randomToken(), expires: Date.now() + this.lifetimeMs };
    this.values.set(id, session);
    return session;
  }
  get(id) {
    const value = this.values.get(id);
    if (value?.expires > Date.now()) return value;
    if (id) this.values.delete(id);
    return undefined;
  }
  delete(id) { this.values.delete(id); }
}

export class RateLimit {
  constructor({ limit = 10, windowMs = 60_000, capacity = 4096 } = {}) {
    this.limit = limit; this.windowMs = windowMs; this.capacity = capacity; this.buckets = new Map();
  }
  take(key) {
    const now = Date.now();
    let bucket = this.buckets.get(key);
    if (!bucket || bucket.until <= now) {
      if (this.buckets.size >= this.capacity) {
        for (const [id, value] of this.buckets) if (value.until <= now) this.buckets.delete(id);
        if (this.buckets.size >= this.capacity) return false;
      }
      bucket = { count: 0, until: now + this.windowMs }; this.buckets.set(key, bucket);
    }
    return ++bucket.count <= this.limit;
  }
}
