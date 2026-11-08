# WalkingPad menu-bar remote

A menu-bar (macOS) / system-tray (Windows) app for KingSmith WalkingPad treadmills. Click the menu-bar or tray icon to open a popover with a replica of the remote, the live speed, today's steps and distance, and a 7-day step chart.

Built with Tauri 2: the UI is plain HTML/CSS/JS in [ui/](ui/), and the core is Rust in [src-tauri/src/](src-tauri/src/):

| File | What |
|---|---|
| `protocol.rs` | Frame encode/decode for the WiLink FE00 protocol. Tests use frames captured from a real pad. See [PROTOCOL.md](PROTOCOL.md). |
| `pad.rs` | Bluetooth worker (btleplug): find/connect/reconnect, 1 Hz polling, debounced speed changes, start sequencing, keep-running blips, stats feeding. No Tauri dependency. |
| `stats.rs` | Daily totals in `stats.json` (app data dir). Converts per-session pad counters into daily deltas and reconciles with the pad's stored last record. |
| `lib.rs` | Tauri shell: tray icon, popover positioning, hide-on-blur, IPC commands. |

Platform differences live in `tauri.macos.conf.json` / `tauri.windows.conf.json` and a few `cfg!(target_os)` branches in `lib.rs`. The macOS menu bar shows a template icon plus the current speed as text. The Windows tray shows the colour icon, with speed and today's steps in the tooltip, and the popover opens above the taskbar.

## Controls

- **+ / −**: ±0.5 km/h per click (shift-click or ↑/↓ keys: ±0.1). Rapid clicks merge into a single command, so the pad beeps only once.
- **Start/stop** (space): from standby it switches to manual first, then starts.
- **Mode**: click toggles manual ↔ auto; press and hold for standby.
- **Keep running when idle**: if no steps register for 20 s, the speed is bumped +0.5 km/h for 1.5 s so the pad doesn't auto-stop. Costs two beeps; the bump's phantom steps aren't counted.

## Develop

```sh
export PATH=/opt/homebrew/opt/rustup/bin:$PATH   # rustup from Homebrew is keg-only
npm install
npm run dev                       # run with hot reload
npm run build -- --bundles app    # build src-tauri/target/release/bundle/macos/WalkingPad.app
cd src-tauri && cargo test        # protocol + stats tests
cargo run --example drive         # hardware smoke test: starts the belt, changes speed, keep-alive, stops
```

Only one Bluetooth central can talk to the pad at a time. Close KS Fit (and this app) before running the smoke test.

UI preview without the app: open `ui/index.html` in a browser (it renders sample data).

## Releases

GitHub Actions (`.github/workflows/`):

- **CI** runs clippy and the unit tests on macOS and Windows on every push and PR.
- **Release** builds a universal macOS `.dmg` (Apple Silicon and Intel) and a Windows NSIS `-setup.exe`. Trigger it by bumping `version` in `src-tauri/tauri.conf.json` and pushing a matching tag. It creates a **draft** release with both installers attached; publish it on GitHub. Running it manually from the Actions tab only uploads the installers as workflow artifacts.

```sh
git tag v0.1.0 && git push origin v0.1.0
```

Neither build is code-signed by a paid certificate. The Mac app is ad-hoc signed, so macOS asks once under Privacy & Security → Open Anyway. Windows SmartScreen shows "More info → Run anyway". Notarization can be added later via the `APPLE_*` secrets that tauri-action supports.

Building the `.dmg` locally needs your terminal to have Automation permission for Finder. `npm run build -- --bundles app` skips the DMG.
