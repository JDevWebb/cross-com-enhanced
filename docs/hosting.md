# Hosting a Cross-Com server

The server is one program with three ports. Run it on a machine your players can reach: a Linux
PC at home with the ports forwarded, a cheap VPS, or a machine on the LAN or VPN you play over.

| Port | Protocol | What for |
|---|---|---|
| 80 | TCP | The online config the game asks for first (it always uses port 80), and sign-up |
| 21030 | UDP | Sign-in |
| 21031 | UDP | Everything else: profiles, matchmaking, friends |

Matches themselves are hosted by a player's game, peer to peer; the server only introduces the
players. The hosting player's router has to let the others in, as it did with Ubisoft's servers.

Players must reach the server at its own address, not through a NAT that changes where their
packets seem to come from: the server tells players each other's addresses as it sees them.
Docker Desktop on macOS and Windows does that rewriting, so host on Linux (or on a VM's bridged
network) for games between different PCs.

## With Docker (recommended)

You need Docker with the compose plugin. In a copy of this repository, write the settings to
`docker/.env` (compose reads it for every command; it isn't committed):

```sh
cat > docker/.env <<END
CROSSCOM_PUBLIC_ADDRESS=203.0.113.10
CROSSCOM_SIGNUP=true
CROSSCOM_SIGNUP_CODE=$(openssl rand -base64 12)
END
docker compose -f docker/compose.yaml up -d
```

`CROSSCOM_PUBLIC_ADDRESS` is the address players connect to: the machine's public IP, or its
address on your LAN or VPN. A host name works too; the server looks it up as it starts.

The database and the optional settings file live in the `data` volume. The server runs as its own
user on a read-only image, with no capabilities; compose lets that user bind port 80.

Logs: `docker compose -f docker/compose.yaml logs -f`.

### Settings

In `docker/.env` (or the environment):

| Variable | Default | |
|---|---|---|
| `CROSSCOM_PUBLIC_ADDRESS` | (required) | The address players connect to |
| `CROSSCOM_SIGNUP` | `false` | `true` lets players make their own accounts |
| `CROSSCOM_SIGNUP_CODE` | (none) | A code players must give to sign up: at least 12 random characters |
| `CROSSCOM_LOG_LEVEL` | `info` | `error`, `warning`, `info`, `debug` or `trace` |

Everything else can go in `crosscom-server.toml` in the volume (see below); the environment wins
over the file.

## Without Docker

Build with the pinned toolchain (`rustup` picks it from `rust-toolchain.toml`):

```sh
cargo build --release -p crosscom-server
```

Then write `crosscom-server.toml` next to where you run it:

```toml
public_address = "203.0.113.10"
database = "crosscom.db"
log_level = "info"
# signup = true
# signup_code = "at least 12 random characters"
```

and run `target/release/crosscom-server --config crosscom-server.toml`. Port 80 needs root or, on
Linux, `sudo setcap cap_net_bind_service=+ep target/release/crosscom-server`.

## News

The game shows news in its main menu (the Ghost Feed on the right) and on its Community Messages
screen. Put it in `news.toml` next to the database (or the file `news = "..."` names in
`crosscom-server.toml`); the server reads it each time a player signs in, so there's no restart:

```toml
[[news]]
title = "Double XP weekend"          # up to 60 characters
body = "All weekend, every ranked match."   # up to 500 bytes
date = 2026-11-20                    # shown as the date; today if left out
shows = "multiplayer"                # or "campaign" or "guerrilla"
```

The first item without `shows` appears in every view; others without it only under Community
Messages. The game takes up to 14. With no `news.toml`, players see a short welcome.

## Accounts

Each player needs an account: a name (letters, digits, `-`, `_`, `.`; up to 32) and a password
(up to 79 characters). The name is what others see in game.

**Made by you:**

```sh
docker compose -f docker/compose.yaml exec server /app/crosscom-server \
  --config /srv/crosscom/crosscom-server.toml add-account NAME PASSWORD
```

(Without Docker: `crosscom-server --config crosscom-server.toml add-account NAME PASSWORD`.)

**Made by players**, when sign-up is on: the launcher does it, or by hand:

```sh
curl -X POST http://SERVER/accounts \
  -H 'Content-Type: application/json' \
  -d '{"name": "Kozak", "password": "their-password", "code": "the-signup-code"}'
```

It answers `201` with the account's ID, or an error with `{"error": "..."}`: `403` without the
right code, `409` if the name is taken, `400` for a name or password the game can't use, `429`
after too many tries (10 a day from one address; 200 sign-ups an hour in all, counting only
those with the right code).

Sign-up goes over plain HTTP, like the game's own sign-in to the server, so passwords cross the
network unencrypted. Players should use a password they use nowhere else. If you leave sign-up
open without a code, anyone who can reach the server can make accounts. The server refuses a code
shorter than 12 characters; make it random (`openssl rand -base64 12`), not a word.

## Checking it works

`crosscom-testbot` signs in the way the game does and checks each step:

```sh
cargo run -p crosscom-testbot -- SERVER_ADDRESS NAME PASSWORD
cargo run -p crosscom-testbot -- SERVER_ADDRESS NEWNAME PASSWORD --signup CODE   # sign-up too
```

Two bots, or a bot and the game, test matchmaking: one runs with `--host 60`, the other with
`--join`.

## Upgrading

```sh
git pull
docker compose -f docker/compose.yaml up -d --build
```

Accounts and statistics stay in the volume.
