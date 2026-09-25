1. Start `npm run tauri dev` and record the app PID with `pgrep -fl agentic-factory`.
2. Close the main window with its red traffic-light button. Confirm the window disappears and the same PID remains.
3. Reopen the window from the tray/menu. Confirm the same PID and a visible, focused window.
4. Choose Quit from the tray/menu. Confirm the PID exits.

## Recorded status — 2026-09-26

- Baseline scaffold: closing its only window with Cmd-W ended the process.
- Resident handler: the packaged Agentic Factory app hid after Cmd-W; the CUA surface reported no visible windows, and pgrep -fl agentic-factory still showed PID 30593.
- The tray Open and Quit menu actions are implemented and compiled. The desktop automation surface does not expose the menu-bar extra after the window is hidden, so steps 3 and 4 still need a direct manual click on macOS.
