import { spawn } from 'node:child_process';
import { resolve, isAbsolute, dirname } from 'node:path';

/** Native validator only. Its environment deliberately excludes gateway/cloud secrets. */
export function runValidator(config, input, browserOut) {
  if (!isAbsolute(config.validatorExecutable)) throw new Error('Validator executable must be absolute');
  if (config.jcoExecutable && !isAbsolute(config.jcoExecutable)) throw new Error('Jco executable must be absolute');
  const args = [...(config.validatorArgs ?? []), '--input', resolve(input), '--browser-out', resolve(browserOut)];
  const command = config.validatorResourceLimits === false ? config.validatorExecutable : '/usr/bin/prlimit';
  const actualArgs = config.validatorResourceLimits === false ? args : [
    '--cpu=20', '--fsize=134217728', '--nofile=128', '--core=0', '--', config.validatorExecutable, ...args,
  ];
  return new Promise((resolveResult, reject) => {
    const child = spawn(command, actualArgs, {
      cwd: browserOut, detached: true, shell: false, stdio: ['ignore', 'pipe', 'pipe'],
      env: { PATH: `${dirname(process.execPath)}:/usr/local/bin:/usr/bin:/bin`, LANG: 'C.UTF-8',
        ...(config.jcoExecutable ? { HORIZON_JCO: config.jcoExecutable } : {}) },
    });
    let stdout = '', stderrBytes = 0, settled = false;
    const kill = () => { try { process.kill(-child.pid, 'SIGKILL'); } catch { child.kill('SIGKILL'); } };
    const finish = (error, result) => {
      if (settled) return;
      settled = true; clearTimeout(timer);
      if (error) { kill(); reject(error); } else resolveResult(result);
    };
    const timer = setTimeout(() => finish(new Error('Mod validation timed out')), config.validatorTimeoutMs ?? 30_000);
    child.on('error', () => finish(new Error('Native mod validator could not start')));
    child.stdout.on('data', bytes => {
      if (Buffer.byteLength(stdout) + bytes.length > 256 * 1024) return finish(new Error('Validator output exceeded its limit'));
      stdout += bytes.toString('utf8');
    });
    child.stderr.on('data', bytes => {
      stderrBytes += bytes.length;
      if (stderrBytes > 256 * 1024) finish(new Error('Validator diagnostics exceeded their limit'));
    });
    child.on('close', code => {
      if (settled) return;
      let result;
      try { result = JSON.parse(stdout); } catch { return finish(new Error('Validator returned an invalid result')); }
      if (code !== 0 || result?.ok !== true) return finish(new Error('Native mod validation rejected the upload'));
      finish(null, result);
    });
  });
}
