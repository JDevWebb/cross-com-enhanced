# The server

`crosscom-server` answers the game's online services:

| Service | Port | What it does |
|---|---|---|
| Online config | TCP 80 (HTTP) | Tells the game where to sign in |
| Auth | UDP 21030 | Signs players in and gives them a ticket for the secure server |
| Secure | UDP 21031 | The game's online services |

So far it signs players in, registers them on the secure server and gives them their privileges. Every other call is logged as unhandled, which is how the next ones to implement are found.

## Running it

```sh
cargo build --release -p crosscom-server
crosscom-server add-account NAME PASSWORD     # each player's account
crosscom-server                               # serves until stopped
```

Settings are in `crosscom-server.toml` ([example](crosscom-server.example.toml)); set `public_address` to the address players reach the server at. Players put the same name and password in their `crosscom.toml`.

## Testing without the game

`crosscom-testbot` signs in the way the game does and checks each step:

```sh
crosscom-testbot SERVER_IP NAME PASSWORD [HTTP_PORT]
```

## Protocols

`grfs_protocols` is generated from the protocol declarations in the game, with the IDs in `data/protocol_ids.json` (see `docs/protocol.md`). Regenerate it rather than editing it:

```sh
ddl-parser -i data/protocol_ids.json -g server/grfs_protocols/src/protocols "<game folder>/Future Soldier DX11.exe"
cargo fmt -p grfs_protocols
```
