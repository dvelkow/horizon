import { connect } from 'node:net';
import { createHash } from 'node:crypto';

function varint(bytes, offset = 0, maxBytes = 5) {
  let value = 0;
  for (let i = 0; i < maxBytes; i++) {
    if (offset + i >= bytes.length) return null;
    const byte = bytes[offset + i]; value |= (byte & 0x7f) << (7 * i);
    if (!(byte & 0x80)) return { value, end: offset + i + 1 };
  }
  throw new Error('Invalid protocol integer');
}
function frame(bytes) {
  const size = varint(bytes, 0, 3);
  if (!size || bytes.length < size.end + size.value) return null;
  if (size.value < 1 || size.value > 8192) throw new Error('Invalid initial packet size');
  return { body: bytes.subarray(size.end, size.end + size.value), end: size.end + size.value };
}
function string(bytes, offset, limit) {
  const length = varint(bytes, offset);
  if (!length || length.value < 0 || length.value > limit * 4 || length.end + length.value > bytes.length) throw new Error('Invalid protocol string');
  const value = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(length.end, length.end + length.value));
  if (value.length > limit) throw new Error('Protocol string is too long');
  return { value, end: length.end + length.value };
}
function offlineUuid(name) {
  const bytes = createHash('md5').update(`OfflinePlayer:${name}`).digest();
  bytes[6] = (bytes[6] & 0x0f) | 0x30; bytes[8] = (bytes[8] & 0x3f) | 0x80;
  return bytes;
}

/** Validate identity before opening the fixed upstream; never trust packet host/port. */
export function initialPackets(bytes, playerName, protocol = 777) {
  const first = frame(bytes);
  if (!first) return null;
  const id = varint(first.body), version = id && varint(first.body, id.end);
  if (!version || id.value !== 0 || version.value !== protocol) throw new Error('Unsupported game handshake');
  const host = string(first.body, version.end, 255);
  const intent = varint(first.body, host.end + 2);
  if (!intent || intent.end !== first.body.length || ![1, 2].includes(intent.value)) throw new Error('Invalid handshake intent');
  if (intent.value === 1) return true;
  const second = frame(bytes.subarray(first.end));
  if (!second) return null;
  const loginId = varint(second.body);
  if (!loginId || loginId.value !== 0) throw new Error('Login hello is required');
  const name = string(second.body, loginId.end, 16);
  if (name.value !== playerName || second.body.length !== name.end + 16 || !second.body.subarray(name.end).equals(offlineUuid(playerName))) throw new Error('Player identity does not match the signed-in session');
  return true;
}

export function bridgeGame(ws, session, config) {
  let upstream, initial = Buffer.alloc(0), ready = false, connecting = false, closed = false;
  const timer = setTimeout(() => stop(1008, 'Game handshake timed out'), config.handshakeTimeoutMs ?? 10_000);
  const expiry = setTimeout(() => stop(1008, 'Game session expired'), Math.max(1, session.expires - Date.now()));
  const stop = (code = 1011, reason = 'Game connection ended') => {
    if (closed) return; closed = true; clearTimeout(timer); clearTimeout(expiry);
    upstream?.destroy();
    if (ws.readyState === ws.OPEN) ws.close(code, reason); else ws.terminate();
  };
  ws.on('error', () => stop());
  ws.on('close', () => { if (!closed) { closed = true; clearTimeout(timer); clearTimeout(expiry); upstream?.destroy(); } });
  ws.on('message', (data, binary) => {
    if (closed) return;
    if (!binary) return stop(1003, 'Binary game messages required');
    if (ready) {
      if (upstream.writableLength + data.length > 4 * 1024 * 1024) return stop(1009, 'Game send buffer exceeded');
      if (!upstream.write(data)) ws.pause();
      return;
    }
    if (initial.length + data.length > 16 * 1024) return stop(1009, 'Game handshake exceeded its limit');
    initial = Buffer.concat([initial, data]);
    if (connecting) return;
    try { if (!initialPackets(initial, session.name, config.protocol ?? 777)) return; }
    catch { return stop(1008, 'Game handshake or identity rejected'); }
    ws.pause();
    connecting = true;
    // Destination is deliberately a literal loopback address, never a query parameter.
    upstream = connect({ host: '127.0.0.1', port: config.gamePort, timeout: 10_000 });
    upstream.setNoDelay(true);
    upstream.on('connect', () => {
      clearTimeout(timer); ready = true;
      upstream.setTimeout(config.gameIdleTimeoutMs ?? 90_000);
      const writable = upstream.write(initial); initial = Buffer.alloc(0);
      if (writable) ws.resume();
    });
    upstream.on('drain', () => ws.resume());
    upstream.on('timeout', () => stop(1013, 'Game server timed out'));
    upstream.on('error', () => stop(1013, 'Game server unavailable'));
    upstream.on('close', () => stop(1000, 'Game server closed the connection'));
    upstream.on('data', bytes => {
      if (ws.readyState !== ws.OPEN) return stop();
      if (ws.bufferedAmount + bytes.length > 4 * 1024 * 1024) return stop(1009, 'Game receive buffer exceeded');
      upstream.pause();
      ws.send(bytes, { binary: true }, error => { if (error) stop(); else if (!closed) upstream.resume(); });
    });
  });
  return () => stop(1001, 'Gateway stopping');
}
