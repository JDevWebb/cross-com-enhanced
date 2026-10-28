# The client

Two DLLs that replace the game's Ubisoft libraries, in the game's folder:

| File | Replaces | What it does |
|---|---|---|
| `ubiorbitapi_r2_loader.dll` | Orbit | Gives the game the account, password and CD key in `crosscom.toml`; sends the game's lookup of `onlineconfigservice.ubi.com` to the Cross-Com server. Cloud saves are accepted and dropped. |
| `uplay_r1_loader.dll` | Uplay | Answers the six Uplay functions the game calls, without Uplay. |

Both read `crosscom.toml` ([example](crosscom.example.toml)) and log to `crosscom.log`, next to the game's exe. They work with both `Future Soldier DX9.exe` and `Future Soldier DX11.exe`.

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
