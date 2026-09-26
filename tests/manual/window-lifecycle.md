1. Start `npm run tauri dev` and record the app PID with `pgrep -fl agentic-factory`.
2. Close the main window with its red traffic-light button. Confirm the window disappears and the same PID remains.
3. Reopen the window from the tray/menu. Confirm the same PID and a visible, focused window.
4. Choose Quit from the tray/menu. Confirm the PID exits.

## Recorded status — 2026-09-26

- Baseline scaffold: closing its only window with Cmd-W ended the process.
- Packaged release: closing the main window hid it while PID 49185 remained alive with its Codex App Server child.
- Tray Open: selected directly from the macOS tray menu; it restored the same PID and the completed `hello` output.
- Tray Quit: selected directly from the macOS tray menu; PID 49185 and its App Server child exited.
- Restart recovery: the final release reopened as PID 55617 and displayed the same `Completed` session and `hello` output. The SQLite ledger still contained exactly five ordered events (`session_created`, `session_started`, `turn_started`, `output`, `turn_completed`), and no Codex App Server process started on relaunch.
