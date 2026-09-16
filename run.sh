#!/usr/bin/env bash
# Build and run the AirCtx server in Docker (data persists in the airctx-data volume).
#
# Usage:
#   ./run.sh                     build image (cached) + start container on :8787
#
# First start prints the security URL (docker logs airctx).
# Open it, register the admin, copy the CLI token, then:
#   airctx login http://127.0.0.1:8787/<gate> <token>
set -euo pipefail
cd "$(dirname "$0")"

# docker daemon may not be running (colima on macOS)
if ! docker info >/dev/null 2>&1; then
  if command -v colima >/dev/null 2>&1; then
    echo "==> docker daemon not running, starting colima..."
    colima start
  else
    echo "error: docker daemon is not running" >&2
    exit 1
  fi
fi

# Colima VM often has /etc/resolv.conf → /run/systemd/resolve/stub-resolv.conf
# while systemd-resolved is not running, so Docker Hub lookups hit [::1]:53 and fail.
fix_colima_dns() {
  command -v colima >/dev/null 2>&1 || return 0
  colima status >/dev/null 2>&1 || return 0
  if colima ssh -- getent hosts registry-1.docker.io >/dev/null 2>&1; then
    return 0
  fi
  echo "==> colima DNS is broken (no systemd-resolved stub); writing public resolvers..."
  colima ssh -- sudo mkdir -p /run/systemd/resolve
  colima ssh -- sudo sh -c 'printf "nameserver 223.5.5.5\nnameserver 1.1.1.1\nnameserver 8.8.8.8\n" > /run/systemd/resolve/stub-resolv.conf'
  if ! colima ssh -- getent hosts registry-1.docker.io >/dev/null 2>&1; then
    echo "warning: still cannot resolve registry-1.docker.io; image pulls may fail" >&2
  fi
}
fix_colima_dns

# a bare (non-docker) `airctx serve` would fight the container port mapping
if pgrep -f "target/release/airctx serve" >/dev/null 2>&1; then
  echo "==> a bare (non-docker) airctx server is running:"
  pgrep -fl "target/release/airctx serve" || true
  read -r -p "    kill it so the container can bind :8787? [y/N] " answer || true
  if [[ "${answer:-}" =~ ^[Yy]$ ]]; then
    pkill -f "target/release/airctx serve" || true
    sleep 1
  else
    echo "    keeping it; the container may fail to bind :8787" >&2
  fi
fi

echo "==> building image (cached; first build takes a few minutes)..."
docker compose up -d --build

echo
docker ps --filter "name=airctx" --format "table {{.Names}}\t{{.Status}}\t{{.Ports}}"
echo
echo "logs:  docker logs airctx   (prints the security URL; register there, copy the token)"
echo "stop:  docker compose down  (add -v to also wipe data)"
