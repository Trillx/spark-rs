#!/usr/bin/env bash
# Run a command in the cross-build container with the repo mounted at /work.
#   tools/cross/run.sh cargo xwin build --release --target aarch64-pc-windows-msvc -p spark-cli
#   tools/cross/run.sh cargo test
# Extra `docker run` arguments (e.g. a network for test servers) go in
# SPARK_CROSS_DOCKER_ARGS.
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
image="spark-rs-cross:1.99.1"
cache="${SPARK_CROSS_CACHE:-$HOME/.cache/spark-rs-cross}"

if ! docker image inspect "$image" >/dev/null 2>&1; then
  docker build -t "$image" "$repo/tools/cross"
fi

mkdir -p "$cache/cargo" "$cache/xwin"
exec docker run --rm \
  --user "$(id -u):$(id -g)" \
  -v "$repo:/work" -w /work \
  -v "$cache:/cache" \
  -e HOME=/cache \
  ${SPARK_CROSS_DOCKER_ARGS:-} \
  "$image" "$@"
