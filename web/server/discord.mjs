import { createHash } from 'node:crypto';
import { randomToken, equalToken } from './security.mjs';

const API = 'https://discord.com/api/v10';
export function oauthCookie(name, value, seconds, secure) {
  return `${name}=${value}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${seconds}${secure ? '; Secure' : ''}`;
}

/** Authorization code flow: state + browser binding + PKCE, server-only tokens. */
export class DiscordAuth {
  constructor(config, origin, fetcher = fetch) {
    if (!/^[0-9]{17,20}$/.test(config.clientId ?? '') || typeof config.clientSecret !== 'string' || config.clientSecret.length < 16) throw new Error('Discord application credentials are missing');
    this.config = config; this.redirect = `${origin}/auth/discord/callback`; this.fetch = fetcher; this.flows = new Map();
  }
  begin() {
    for (const [key, value] of this.flows) if (value.expires <= Date.now()) this.flows.delete(key);
    if (this.flows.size >= 1024) throw new Error('Too many pending Discord sign-ins');
    const state = randomToken(), binding = randomToken(), verifier = randomToken();
    this.flows.set(state, { binding, verifier, expires: Date.now() + 10 * 60 * 1000 });
    const query = new URLSearchParams({ client_id: this.config.clientId, response_type: 'code', scope: 'identify', redirect_uri: this.redirect, state,
      code_challenge: createHash('sha256').update(verifier).digest('base64url'), code_challenge_method: 'S256' });
    return { binding, url: `https://discord.com/oauth2/authorize?${query}` };
  }
  async json(url, options) {
    const response = await this.fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(10_000) });
    if (!response.ok) throw new Error('Discord request failed');
    const chunks = []; let size = 0;
    for await (const chunk of response.body) {
      size += chunk.length;
      if (size > 64 * 1024) throw new Error('Discord response exceeded its limit');
      chunks.push(chunk);
    }
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
  }
  async finish(query, binding) {
    const state = query.get('state'), flow = this.flows.get(state);
    if (!flow || !equalToken(binding, flow.binding) || flow.expires <= Date.now()) throw new Error('Discord sign-in expired');
    this.flows.delete(state); // Consume even cancelled/failed attempts; no code replay.
    if (query.has('error')) return null;
    if (query.has('iss') && query.get('iss') !== 'https://discord.com') throw new Error('Invalid authorization issuer');
    const code = query.get('code');
    if (!code || code.length > 2048) throw new Error('Missing authorization code');
    const tokens = await this.json(`${API}/oauth2/token`, { method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({ client_id: this.config.clientId, client_secret: this.config.clientSecret, grant_type: 'authorization_code', code,
        redirect_uri: this.redirect, code_verifier: flow.verifier }).toString() });
    if (tokens.token_type?.toLowerCase() !== 'bearer' || typeof tokens.access_token !== 'string' || !tokens.scope?.split(' ').includes('identify')) throw new Error('Discord did not authorize identity');
    return this.json(`${API}/users/@me`, { headers: { Authorization: `Bearer ${tokens.access_token}` } });
  }
}
