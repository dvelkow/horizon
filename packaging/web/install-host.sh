#!/usr/bin/env bash
# Run as root on the dedicated Horizon host. Cloud credentials are never needed here.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq caddy libasound2t64 libudev1 rsync xz-utils ca-certificates curl
if ! id horizon >/dev/null 2>&1; then useradd --system --create-home --home-dir /var/lib/horizon --shell /usr/sbin/nologin horizon; fi
install -d -m 755 /opt/horizon/{bin,resources,web,gateway,mod-tools}
install -d -o horizon -g horizon -m 700 /var/lib/horizon/game /var/lib/horizon-portal /etc/horizon
if [ ! -f /swapfile ]; then
  fallocate -l 2G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile
  swapon /swapfile
  printf '/swapfile none swap sw 0 0\n' >> /etc/fstab
fi
# Pin the exact downloaded Node 22 archive using its official SHA-256 manifest.
if ! /usr/local/bin/node --version 2>/dev/null | grep -q '^v22\.'; then
  stage=$(mktemp -d)
  trap 'rm -rf "$stage"' EXIT
  curl -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt -o "$stage/SHASUMS256.txt"
  archive=$(awk '/ node-v[0-9.]+-linux-x64.tar.xz$/ {print $2}' "$stage/SHASUMS256.txt")
  test -n "$archive"
  curl -fsSL "https://nodejs.org/dist/latest-v22.x/$archive" -o "$stage/$archive"
  (cd "$stage"; grep " $archive$" SHASUMS256.txt | sha256sum -c -)
  mkdir -p /opt/horizon/node
  tar -xJf "$stage/$archive" --strip-components=1 -C /opt/horizon/node
  ln -sfn /opt/horizon/node/bin/node /usr/local/bin/node
fi
