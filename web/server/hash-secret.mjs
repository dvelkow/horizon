import { hashSecret } from './security.mjs';

// Read a private secret from stdin, never from arguments or shell history.
let secret = '';
for await (const chunk of process.stdin) {
  secret += chunk;
  if (secret.length > 1025) throw new Error('Secret input exceeds its limit');
}
console.log(await hashSecret(secret.replace(/\r?\n$/, '')));
