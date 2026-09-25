# Agentic Factory

Agentic Factory keeps one Codex App Server conversation resident in a macOS Tauri app. The window can hide while the Rust process, Codex session, and local event ledger remain alive. This repository implements the first resident-session slice only; the broader multi-agent roadmap remains in `outputs/agentic-factory-implementation-plan.md`.

## Run locally

```sh
npm install
npm run tauri dev
```

The app uses the installed `codex` CLI on the same Mac. It stores local session state under the app's local data directory. The session runner must use a read-only disposable repository; do not point it at a user project.

## Build and verify

```sh
npm run build
cargo test --workspace
npm run tauri build
```

The window and tray acceptance procedure is in `tests/manual/window-lifecycle.md`. The harmless App Server protocol probe and its observed event names are in `tests/manual/app-server-probe.md`.

## Toolchain recorded during scaffold

- macOS SDK: 27.0; generated Mach-O minimum deployment target: macOS 11.0 (`LC_BUILD_VERSION` `minos`).
- Rust `1.96.0` and Cargo `1.96.0`.
- Node.js `24.11.0` and npm `11.6.1`.
- Tauri Rust crate `2.11.6`, CLI `2.11.5`, and JavaScript API `2.11.1`.
- React and React DOM `19.3.0`, Vite `8.3.1`, and TypeScript `6.0.3`.
- Codex CLI App Server probe: `codex-cli 0.157.0`.

`package-lock.json` and the workspace `Cargo.lock` pin resolved dependencies. The app currently targets macOS; other platforms are outside this plan slice.
