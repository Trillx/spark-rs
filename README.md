<img src="assets/icon.svg" width="64" height="64" alt="">

# spark-rs

A personal, Spark-style email client with a Rust core. First target: Windows
on ARM (`aarch64-pc-windows-msvc`).

Created in [T3 Code](https://t3.codes).

## Layout

```
crates/store              SQLite storage: accounts, messages, outbox (bundled SQLite with FTS5)
crates/mail               IMAP/SMTP and Microsoft Graph: sync, sending, Microsoft sign-in
crates/ai                 bring-your-own-key AI client (Anthropic, OpenAI-compatible)
apps/cli                  spark-cli, the headless dev tool
apps/desktop              desktop app UI: React + Tailwind + daisyUI (Vite)
apps/desktop/src-tauri    desktop app shell (Tauri 2): accounts, sync workers, demo mailbox
tools/cross               Docker image for building Windows binaries on Linux
```

## Building

Everything builds in a Docker image, so the host needs nothing besides
Docker. The first run builds the image (a few minutes). On the first Windows
build, cargo-xwin downloads the MSVC CRT and Windows SDK (about 5 GB, cached in
`~/.cache/spark-rs-cross`); using those means accepting Microsoft's license
terms for them.

```sh
# Linux: lint, test, run
tools/cross/run.sh cargo clippy --workspace --all-targets -- -D warnings
tools/cross/run.sh cargo test --workspace
tools/cross/run.sh cargo run -p spark-cli -- doctor

# IMAP/SMTP end-to-end test against GreenMail (setup in crates/mail/tests/greenmail.rs)
SPARK_CROSS_DOCKER_ARGS='--network spark-test -e SPARK_TEST_GREENMAIL=spark-greenmail' \
  tools/cross/run.sh cargo test -p spark-mail --test greenmail

# Desktop UI: build it first (Node on the host); the app embeds apps/desktop/dist
(cd apps/desktop && npm install && npm run build)
(cd apps/desktop && npm run dev)   # UI alone in a browser at localhost:1420, demo data

# Windows on ARM
tools/cross/run.sh cargo xwin build --release --target aarch64-pc-windows-msvc -p spark-desktop -p spark-cli
# → target/aarch64-pc-windows-msvc/release/{spark-desktop,spark-cli}.exe
```

`spark-desktop.exe` is a single file with the UI embedded; it needs the
WebView2 runtime, which Windows 11 includes. On the Windows machine,
`spark-cli.exe doctor` checks that SQLite, TLS and networking work there.

TLS uses rustls with the aws-lc-rs backend. Don't switch it to `ring`: ring's
build forces plain `clang` on ARM64 Windows, which fails with cargo-xwin's
MSVC-style flags.
