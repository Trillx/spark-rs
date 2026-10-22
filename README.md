<img src="assets/icon.svg" width="64" height="64" alt="">

# spark-rs

A personal, Spark-style email client with a Rust core. First target: Windows
on ARM (`aarch64-pc-windows-msvc`).

Created in [T3 Code](https://t3.codes).

## Accounts

Settings (`Ctrl+,`) → Accounts → Add an account. Until the first account is
added the app shows a demo mailbox.

Mail is stored in `mail.sqlite` in the app's local data folder
(`%LOCALAPPDATA%\dev.sparkrs.desktop` on Windows). Passwords and Microsoft
sign-ins are kept in Windows Credential Manager under `spark-rs`, never in
files. Removing an account deletes its local mail and saved sign-in; nothing
is deleted on the server.

**Gmail.** Google requires an app password for mail apps: turn on 2-Step
Verification, create one at <https://myaccount.google.com/apppasswords>, then
choose **Gmail** and enter your address and the 16 letters. spark-rs uses
`imap.gmail.com:993` and `smtp.gmail.com:465` and checks the login before
saving.

**Microsoft 365 (work or school).** This needs an app registration in your
organization's Microsoft Entra tenant (<https://entra.microsoft.com> → App
registrations → New registration). If you can't register apps, ask IT:

- Supported account types: this organizational directory only.
- Platform: **Mobile and desktop applications**, redirect URI
  `http://localhost` (any port matches; spark-rs picks a free one).
- API permissions, Microsoft Graph, delegated: `User.Read`,
  `Mail.ReadWrite`, `Mail.Send`, `offline_access`. If your tenant requires
  admin consent, IT has to grant it.
- No client secret: desktop apps are public clients.

Then choose **Microsoft 365** and enter the Application (client) ID and the
Directory (tenant) ID, or your domain such as `contoso.onmicrosoft.com`.
Your browser opens Microsoft's sign-in; when it's done the account appears.
Check your company's rules first: using your own client on work mail, and
sending work mail to an AI provider for summaries, may not be allowed.

**Other IMAP servers.** Choose **Other (IMAP)** and enter the servers. Ports
993 and 465 use TLS from the start; 143 and 587 use STARTTLS.

What syncs: the inbox (the newest 300 messages on Gmail/IMAP, the last 30
days on Microsoft 365), checked every two minutes and on demand. Done,
Delete, read and pinned go back to the server after the 6-second undo
window; Done archives (All Mail on Gmail). Snooze, Set Aside, Priority and
new-sender screening stay on this computer.

Attachments download when you open or save them, also from archived mail.
Program files (`.exe`, `.js`, `.lnk` and the like) can only be saved, not
opened, and downloaded files are marked as coming from the internet, so
Office opens them in Protected View. Attach files with the paperclip,
Ctrl+Shift+A, or by dropping them on the window; forwards keep the original's
attachments. Messages over 10 MB are stored without their body or file list.

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
# Desktop UI first (Node on the host): spark-desktop embeds apps/desktop/dist
# and doesn't compile without it
(cd apps/desktop && npm install && npm run build)
(cd apps/desktop && npm run dev)   # UI alone in a browser at localhost:1420, demo data

# Linux: lint, test, run
tools/cross/run.sh cargo clippy --workspace --all-targets -- -D warnings
tools/cross/run.sh cargo test --workspace
tools/cross/run.sh cargo run -p spark-cli -- doctor

# IMAP/SMTP end-to-end test against GreenMail (setup in crates/mail/tests/greenmail.rs)
SPARK_CROSS_DOCKER_ARGS='--network spark-test -e SPARK_TEST_GREENMAIL=spark-greenmail' \
  tools/cross/run.sh cargo test -p spark-mail --test greenmail

# Windows on ARM
tools/cross/run.sh cargo xwin build --release --target aarch64-pc-windows-msvc -p spark-desktop -p spark-cli
# → target/aarch64-pc-windows-msvc/release/{spark-desktop,spark-cli}.exe
```

`spark-desktop.exe` is a single file with the UI embedded; it needs the
WebView2 runtime, which Windows 11 includes. On the Windows machine,
`spark-cli.exe doctor` checks that SQLite, TLS and networking work there.

### Natively on Windows ARM64

On an ARM64 Windows machine you can skip Docker. You need rustup, Node,
Visual Studio Build Tools 2022 with the **MSVC ARM64 build tools**, and
clang-cl: aws-lc-sys won't build with MSVC's `cl` on ARM64. Either install
LLVM (`winget install LLVM.LLVM`) or add the **C++ Clang Compiler for
Windows** component to the Build Tools. `tools\win-arm64\cargo.cmd` runs
cargo in the ARM64 build environment with clang-cl; it finds cargo in
`%USERPROFILE%\.cargo\bin` and LLVM in its default folder when they aren't on
`PATH`.

From the repo root, in cmd or PowerShell:

```
cd apps\desktop
npm install
npm run build
cd ..\..
tools\win-arm64\cargo.cmd clippy --workspace --all-targets -- -D warnings
tools\win-arm64\cargo.cmd test --workspace
tools\win-arm64\cargo.cmd build --release -p spark-desktop -p spark-cli
```

The binaries land in `target\release\`.

For the GreenMail test without Docker, run the standalone jar with Java 17+
(`java -Dgreenmail.setup.test.all -Dgreenmail.hostname=127.0.0.1
-Dgreenmail.auth.disabled -jar greenmail-standalone-2.1.5.jar`, from Maven
Central) and set `SPARK_TEST_GREENMAIL=127.0.0.1` before `cargo.cmd test`.

TLS uses rustls with the aws-lc-rs backend. Don't switch it to `ring`: ring's
build forces plain `clang` on ARM64 Windows, which fails with cargo-xwin's
MSVC-style flags.
