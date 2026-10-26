#!/usr/bin/env bash
# Called inside tools/cross/run.sh after building the UI on the host.
set -euo pipefail
target="${1:?Windows target triple required}"
case "$target" in
  aarch64-pc-windows-msvc|x86_64-pc-windows-msvc) ;;
  *) echo "Unsupported target: $target" >&2; exit 1 ;;
esac
: "${SPARK_COMMIT:?Set SPARK_COMMIT to the built commit}"
: "${SPARK_VERSION:?Set SPARK_VERSION to X.Y.Z+N}"

cargo xwin build --release --locked --target "$target" -p spark-cli
mkdir -p apps/desktop/src-tauri/binaries
cp "${CARGO_TARGET_DIR:-target}/$target/release/spark-cli.exe" "apps/desktop/src-tauri/binaries/spark-cli-$target.exe"
cd apps/desktop
cargo tauri build --runner cargo-xwin --target "$target" --bundles nsis \
  --config src-tauri/installer.conf.json \
  --config "{\"version\":\"$SPARK_VERSION\"}" -- --locked
