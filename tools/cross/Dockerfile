# Cross-compiles spark-rs for Windows (MSVC ABI) from Linux.
# cargo-xwin downloads the MSVC CRT and Windows SDK on first use; using them
# means accepting Microsoft's license terms for those components.
FROM rust:1.99.1-trixie

# clang/lld/llvm for Windows targets; WebKitGTK and friends so the Tauri app
# also builds (and lints/tests) for Linux.
RUN apt-get update \
 && apt-get install -y --no-install-recommends clang lld llvm \
      pkgconf libwebkit2gtk-4.1-dev libxdo-dev libssl-dev librsvg2-dev \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s clang /usr/bin/clang-cl \
 && for tool in lld-link llvm-lib llvm-rc; do \
      command -v "$tool" >/dev/null || ln -s "$(ls /usr/bin/${tool}-* | sort -V | tail -1)" "/usr/bin/$tool"; \
    done

RUN rustup component add clippy rustfmt \
 && rustup target add aarch64-pc-windows-msvc x86_64-pc-windows-msvc \
 && cargo install --locked cargo-xwin --version 0.23.1 \
 && rm -rf /usr/local/cargo/registry

# The CLI bundles NSIS on Linux. Build the UI on the host before calling it;
# the installer config disables its Node build hook inside this image.
RUN apt-get update \
 && apt-get install -y --no-install-recommends nsis libayatana-appindicator3-dev \
 && rm -rf /var/lib/apt/lists/* \
 && curl --fail --location --retry 3 \
      https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.12.4/cargo-tauri-x86_64-unknown-linux-gnu.tgz \
      -o /tmp/cargo-tauri.tgz \
 && tar -xzf /tmp/cargo-tauri.tgz -C /usr/local/cargo/bin cargo-tauri \
 && rm /tmp/cargo-tauri.tgz

# Caches are mounted from the host so builds run as the host user.
ENV CARGO_HOME=/cache/cargo \
    XWIN_CACHE_DIR=/cache/xwin \
    PATH=/usr/local/cargo/bin:$PATH
