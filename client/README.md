# The client

Two DLLs that replace the game's Ubisoft libraries, in the game's folder:

| File | Replaces | What it does |
|---|---|---|
| `ubiorbitapi_r2_loader.dll` | Orbit | Gives the game the account, password and CD key in `crosscom.toml`; sends the game's lookup of `onlineconfigservice.ubi.com` to the Cross-Com server. Cloud saves are kept on this PC. Draws the in-game overlay. Keeps each run's log, and sends earlier runs' to the server. |
| `uplay_r1_loader.dll` | Uplay | Answers the six Uplay functions the game calls, without Uplay. |

Both read `crosscom.toml` ([example](crosscom.example.toml)) and log to `crosscom.log`, next to the game's exe. They work with both `Future Soldier DX9.exe` and `Future Soldier DX11.exe`.

## The overlay

F5 (or `overlay_key`, F1 to F12) opens a panel over the game; it or Esc closes it. While it's open the game gets no mouse or keyboard. Its pages:

- **Server**: the address from `crosscom.toml`, whether it answers, its version and how many players are online.
- **News**: the server's news.
- **Leaderboards**: every player on the server, by mode (All, Conflict, Decoy, Saboteur, Siege, Stockade, Takeover): wins, losses, kills, deaths, objectives and time played. Your own row is highlighted. The game's own Leaderboards screen (friends only) can't be opened on PC.
- **Account**: the account the game signs in with.
- **Help**: the keys, and how to report a problem.

Toasts in the top-right corner say when the game signs in ("Cross-Com: signed in as NAME"), when the server stops answering, and when there's news.

The server's details come from `GET http://<server>/api/info` (its HTTP port, 80), asked every minute on a thread of its own. Drawing uses [hudhook](https://github.com/veeenu/hudhook) (patched, in `vendor/hudhook`) and Dear ImGui over the game's DirectX 11 or 9 frames, by the exe's name. It starts 10 s after the game does, as hooking earlier can stop a fullscreen game from starting. If it can't hook (no DirectX device it can use, a library missing) it says why in `crosscom.log` and the game runs without it. `overlay = false` turns it off: nothing is hooked and no requests are made.

## Logs

Only `ubiorbitapi_r2_loader.dll` writes `crosscom.log`. As the game starts, before the log opens, the last run's `crosscom.log` moves to `logs\crosscom-<yyyymmdd-hhmmss>.log` (the time, UTC, that run started) and a new one begins; the 20 newest runs are kept. The first lines say the client's version, the exe and the Windows version.

Unless `send_logs = false`, finished runs go to the server (`POST http://<server>/api/logs`, see [docs/hosting.md](../docs/hosting.md#players-logs)), with the Windows user name (also in `C:\Users\<name>`) and the account's password replaced by `<user>` and `<password>`, and anything over 2 MiB cut to its start and end. The launcher sends a run as soon as the game it started exits. A game started without the launcher sends the earlier runs itself, on a thread of its own, 30 s after it asks for its sign-in details (the server only takes logs from an address the account has played from). A run the server took is renamed `*.sent.log`; one it didn't stays for next time, and `crosscom.log` says why ("Logs of the run of ... sent: report ...").

## Building

32-bit Windows DLLs, cross-built with [cargo-xwin](https://github.com/rust-cross/cargo-xwin):

```sh
cargo xwin build --release -p crosscom-orbit -p crosscom-uplay --target i686-pc-windows-msvc
```

## Trying it

1. Back up the game's `ubiorbitapi_r2_loader.dll` and `uplay_r1_loader.dll`, then copy these two over them.
2. Start a server on a machine the game's PC can reach: `scripts/dev-server.sh <that machine's address> NAME PASSWORD` runs one in Docker and creates the account.
3. Save `crosscom.example.toml` as `crosscom.toml` in the game's folder, with `server` set to that address and the same name and password.
4. Start the game and go online. The server logs each step (online config, sign-in, the secure server) and every call it can't answer yet; `crosscom.log` says what the DLLs did.
