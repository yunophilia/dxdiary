#!/usr/bin/env bash
# Build crowsnest for one or more targets inside the toolchain container.
# Docker is the only host requirement -- no Rust, no C compiler.
#
#   ./scripts/build.sh                  # every target
#   ./scripts/build.sh musl             # just the static Linux build
#   ./scripts/build.sh win linux-gnu    # several, by alias
#
# Binaries land in dist/<triple>/. Run from Git Bash on Windows.

set -euo pipefail

cd "$(dirname "$0")/.."

IMAGE=crowsnest-build
# Named volumes, not host directories: cargo's target dir and registry are
# write-heavy, and on Docker Desktop a bind mount crosses a VM boundary that
# makes them crawl. Only the source tree and dist/ are bind-mounted.
VOL_TARGET=crowsnest-target
VOL_REGISTRY=crowsnest-registry

# Linux only -- see the Dockerfile for why there is no Windows target.
declare -A TARGETS=(
  [linux-gnu]=x86_64-unknown-linux-gnu
  [musl]=x86_64-unknown-linux-musl
  [arm64]=aarch64-unknown-linux-gnu
)
DEFAULT_ORDER=(musl linux-gnu arm64)

command -v docker >/dev/null 2>&1 || { echo "error: docker not found" >&2; exit 1; }

echo "=== building toolchain image ==="
docker build -t "$IMAGE" .

selected=("$@")
[ ${#selected[@]} -eq 0 ] && selected=("${DEFAULT_ORDER[@]}")

failed=()
for alias in "${selected[@]}"; do
  target="${TARGETS[$alias]:-$alias}"   # a full triple works too
  echo
  echo "=== $alias -> $target ==="
  mkdir -p "dist/$target"

  if docker run --rm \
      -v "$PWD:/app" \
      -v "$VOL_TARGET:/app/target" \
      -v "$VOL_REGISTRY:/usr/local/cargo/registry" \
      -v "$PWD/dist:/dist" \
      "$IMAGE" \
      bash -c "
        set -eu
        cargo build --release --locked --target '$target'
        # target/ is a volume the host cannot see; copy results out to /dist.
        cp 'target/$target/release/crowsnest' '/dist/$target/'
      "; then
    ls -lh "dist/$target" 2>/dev/null || true
  else
    echo "!!! FAILED: $target" >&2
    failed+=("$target")
  fi
done

echo
if [ ${#failed[@]} -gt 0 ]; then
  echo "failed targets: ${failed[*]}" >&2
  exit 1
fi
echo "done -> dist/"
