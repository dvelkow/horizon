#!/usr/bin/env bash
# Stage a verified browser release; activation is a separate explicit operation.
# The SSH alias must connect as root. No cloud credential or app secret is needed.
set -euo pipefail
usage() {
  echo "Usage: $0 stage SSH_HOST RELEASE_ID [PUBLIC_DIR] | activate SSH_HOST RELEASE_ID" >&2
  exit 2
}
[[ $# -ge 3 ]] || usage
operation=$1 host=$2 release=$3
[[ "$host" =~ ^[A-Za-z0-9_][A-Za-z0-9_.@-]*$ ]] || usage
[[ "$release" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] || usage
[[ "$operation" == stage || "$operation" == activate ]] || usage

if [[ "$operation" == activate ]]; then
  [[ $# == 3 ]] || usage
  ssh "$host" bash -s -- "$release" <<'REMOTE'
set -euo pipefail
[[ $(id -u) == 0 ]] || { echo 'Connect with the operator root SSH alias.' >&2; exit 1; }
release=$1
exec 9>/run/lock/horizon-web-update.lock
flock -x 9
target=/opt/horizon/releases/$release
test -f "$target.ready"
test -d "$target"
(cd "$target"; sha256sum --strict --quiet -c "$target.sha256")
if [[ -e /opt/horizon/web && ! -L /opt/horizon/web ]]; then
  # First migration: stop the gateway before replacing its physical directory.
  # Keep the old tree intact for rollback. World simulation is not restarted.
  legacy=/opt/horizon/releases/legacy-$(date -u +%Y%m%dT%H%M%S)-$$
  systemctl stop horizon-gateway
  mv /opt/horizon/web "$legacy"
  ln -s "$legacy" /opt/horizon/web
fi
previous=$(readlink -f /opt/horizon/web)
test -d "$previous"
link=/opt/horizon/.web-next-$$
trap 'rm -f "$link"' EXIT
ln -s "$target" "$link"
mv -Tf "$link" /opt/horizon/web
# The gateway resolves its roots and owns sessions at startup. Existing players
# must sign in again after this explicit client release activation.
if ! systemctl restart horizon-gateway || ! systemctl is-active --quiet horizon-gateway; then
  ln -s "$previous" "$link"
  mv -Tf "$link" /opt/horizon/web
  systemctl restart horizon-gateway
  echo 'Gateway activation failed; restored the preceding browser release.' >&2
  exit 1
fi
printf 'Activated %s; previous release: %s\n' "$release" "$previous"
echo 'Check HTTPS /healthz, sign in, and join the world. Native game was not restarted.'
REMOTE
  exit
fi

[[ $# -le 4 ]] || usage
source_dir=${4:-.local/web}
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
# Upload only build outputs. A diagnostic page or private file elsewhere in the
# build folder is deliberately not part of this allowlist.
python3 - "$source_dir" "$temporary" <<'PY'
import hashlib, pathlib, re, sys
root, out = map(pathlib.Path, sys.argv[1:])
required = ['index.html', 'app.js', 'wardrobe.js', 'style.css', 'portal.html', 'portal.js',
            'bootstrap.js', 'rust-worker.js', 'pkg/horizon_client.js',
            'pkg/horizon_client_bg.wasm', 'mods-runtime/host.js',
            'mods-runtime/worker.js', 'assets/index.json',
            'branding/pepe-pixel-party-v1.png', 'branding/Geist-SemiBold.ttf',
            'branding/Geist-Regular.ttf', 'branding/OFL-Geist.txt']
required.extend(f'skins/{skin}.png' for skin in ['steve','alex','ari','efe','kai','makena','noor','sunny','zuri'])
selected = set(required)
for folder in ['pkg', 'assets']:
    for path in (root / folder).rglob('*'):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            raise SystemExit('Refusing a symlink in public outputs: ' + relative)
        if not path.is_file():
            continue
        if (folder == 'pkg' and relative.endswith(('.js', '.wasm', '.d.ts'))
            or re.fullmatch(r'assets/[a-f0-9]{64}\.(json|zip)', relative)
            or relative == 'assets/browser-sounds.json'
            or re.fullmatch(r'assets/assets/[a-z0-9_.-]+/sounds/[a-z0-9_./-]+\.ogg', relative)):
            selected.add(relative)
# The build's precompressed copies travel with their files.
for name in list(selected):
    for ext in ('.br', '.gz'):
        if (root / (name + ext)).is_file() and not (root / (name + ext)).is_symlink():
            selected.add(name + ext)
lines, hashes = [], []
for name in sorted(selected):
    path = root / name
    if (not path.is_file() or path.is_symlink()
        or any(part.startswith('.') for part in pathlib.PurePosixPath(name).parts)
        or not re.fullmatch(r'[A-Za-z0-9_./-]+', name)
        or not path.resolve().is_relative_to(root.resolve())):
        raise SystemExit('Missing or unsafe public build file: ' + name)
    lines.append(name + '\n')
    digest = hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
    hashes.append(digest + '  ' + name + '\n')
(out / 'files').write_text(''.join(lines))
(out / 'SHA256SUMS').write_text(''.join(hashes))
print(f'Preparing {len(lines)} public files; no private configuration is included.')
PY
ssh "$host" bash -s -- "$release" <<'REMOTE'
set -euo pipefail
[[ $(id -u) == 0 ]] || { echo 'Connect with the operator root SSH alias.' >&2; exit 1; }
release=$1
install -d -m 755 /opt/horizon/releases
test ! -e "/opt/horizon/releases/$release"
mkdir -m 755 "/opt/horizon/releases/$release.staging"
# Retain old immutable bundles for cached/in-flight clients. rsync replaces files
# through rename (never --inplace), so hardlinked preceding releases stay intact.
if [[ -d /opt/horizon/web/assets ]]; then
  mkdir -m 755 "/opt/horizon/releases/$release.staging/assets"
  cp -al /opt/horizon/web/assets/. "/opt/horizon/releases/$release.staging/assets/"
fi
REMOTE
rsync -rt --checksum --chmod=D755,F644 --files-from="$temporary/files" \
  "$source_dir/" "$host:/opt/horizon/releases/$release.staging/"
rsync -t --chmod=F644 "$temporary/SHA256SUMS" "$host:/opt/horizon/releases/$release.sha256"
ssh "$host" bash -s -- "$release" <<'REMOTE'
set -euo pipefail
release=$1
cd "/opt/horizon/releases/$release.staging"
sha256sum --strict --quiet -c "../$release.sha256"
test -z "$(find . -type l -print -quit)"
# Public build output must be readable by the unprivileged gateway. mkstemp's
# default 0600 mode otherwise turns an apparently successful upload into a 502.
find . -type d -exec chmod 755 {} +
find . -type f -exec chmod 644 {} +
cd ..
mv "$release.staging" "$release"
touch "$release.ready"
chmod 644 "$release.ready"
printf 'Staged %s. No services restarted.\n' "$release"
REMOTE
printf 'Activate explicitly: %q activate %q %q\n' "$0" "$host" "$release"
