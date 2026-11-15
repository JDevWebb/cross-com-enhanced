# Hosting a Cross-Com server

The server is one program with three ports. Run it on a machine your players can reach: a Linux
PC at home with the ports forwarded, a cheap VPS, or a machine on the LAN or VPN you play over.

| Port | Protocol | What for |
|---|---|---|
| 80 | TCP | The online config the game asks for first (it always uses port 80), the launcher's API and sign-up |
| 443 | TCP | Only with [HTTPS](#https-with-caddy): the launcher's sign-in and sign-up, encrypted |
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
| `CROSSCOM_DOMAIN` | (none) | The server's domain, for [HTTPS](#https-with-caddy) (`compose.https.yaml` only) |
| `CROSSCOM_TRUSTED_PROXIES` | (none) | Reverse proxies in front of port 80, by address or network, separated by commas (`compose.https.yaml` sets it) |
| `CROSSCOM_ACCEPT_LOGS` | `true` | `false` turns [players' logs](#players-logs) off |
| `CROSSCOM_LOGS_MAX_MB` | `2048` | The most players' logs may take in all, in MiB; past it the oldest go |
| `CROSSCOM_LOGS_KEEP_DAYS` | `30` | Days players' logs are kept |

Everything else can go in `crosscom-server.toml` in the volume (see below); the environment wins
over the file.

## HTTPS with Caddy

The launcher sends players' passwords when they sign in or make an account, over HTTPS. It
falls back to plain HTTP only for a server set up by a private IP address (`10.x`, `172.16-31.x`,
`192.168.x`, `100.64-127.x` as Tailscale uses, `fc00::/7`, link-local, or this machine) where
nothing answers on port 443 at all, and tells the player their password went unencrypted. A
server set up by name is never sent a password unencrypted, whatever its name points at, nor is
one whose HTTPS answers but fails (a certificate that isn't valid): the launcher refuses and says
the server needs HTTPS. So a LAN or VPN server without a domain is set up in the launcher by its
IP address.

`docker/compose.https.yaml` puts [Caddy](https://caddyserver.com) in front of the server's port
80. Caddy gets a certificate from Let's Encrypt and renews it by itself. You need:

- a domain (or a subdomain; a free dynamic DNS name works) with a DNS `A` record pointing at the
  machine's public IP;
- TCP ports 80 and 443 open or forwarded to the machine, as well as UDP 21030 and 21031;
- Docker Compose 2.24.4 or later.

Add the domain to `docker/.env` and start both files together:

```sh
echo CROSSCOM_DOMAIN=play.example.org >> docker/.env
docker compose -f docker/compose.yaml -f docker/compose.https.yaml up -d
```

Use the same two `-f` files for every other compose command (`logs`, `exec`, `down`). Players can
set up the launcher with the domain or the IP address: the game itself only uses port 80.

What stays on plain HTTP, on port 80, and why:

- `GET /OnlineConfigService.svc/GetOnlineConfig`: the game asks for it there and can't use HTTPS.
- `GET /api/...` (the server's info, news and other read-only answers): the in-game overlay reads
  them and has no TLS. None carries a password.
- `POST /api/logs`: [players' logs](#players-logs), sent by the client DLL, which has no TLS.
  They carry no password, and Caddy takes up to 5 MB of one (the server, 4 MiB).

Everything else on port 80, notably `POST /accounts` and `POST /api/login`, is redirected to
`https://` the domain. Certificates' challenges are answered on port 80 too. The UDP ports aren't
proxied; the game's own sign-in goes there as before.

Caddy's address on the compose network is fixed (`172.31.80.10`), and the server trusts it alone
(`CROSSCOM_TRUSTED_PROXIES`): from it, the server counts each request as coming from the rightmost
`X-Forwarded-For` address, so the sign-in and sign-up limits count each player rather than Caddy,
and it refuses passwords unless `X-Forwarded-Proto` says HTTPS. From any other address those
headers are ignored. If `172.31.80.0/24` is already used on the machine, change the subnet,
Caddy's address and `CROSSCOM_TRUSTED_PROXIES` in `compose.https.yaml` together.

Caddy runs from the official image, pinned by digest, on a read-only filesystem with only the
capability to bind ports 80 and 443. Its certificates live in the `caddy_data` volume; keep it,
or Let's Encrypt's rate limits may bite after a few rebuilds.

Without a domain (a server only on a LAN or VPN), leave `compose.https.yaml` out: the server
answers everything on port 80 as before.

With a reverse proxy of your own instead, have it set `X-Forwarded-For` (appending the client's
address on the right) and `X-Forwarded-Proto`, keep the plain-HTTP paths above on port 80 (with
bodies of 4 MiB allowed on `POST /api/logs`), and name it in `trusted_proxies`.

### Sharing a machine with another game's server

One machine can host this server and another game's server built the same way, such as
[5th Echelon Enhanced](https://github.com/JDevWebb/5th-echelon-enhanced) for Splinter Cell:
Blacklist. Their game ports differ (UDP 21030 and 21031 here, 21126 to 21128 there); what they
share is TCP 80 and 443, where both games ask for their online config, at the same path. One
Caddy holds both ports and tells the games apart: Future Soldier's request carries its own config
ID (`23ad683803a0457cabce83f905811dbc`), so it comes here and every other goes to the other
server. Launchers, overlays and tools ask by domain, one per server.

You need:

- two domains (or subdomains) pointing at the machine: one for each server;
- the other server's HTTP side (its online config and API) moved off port 80, to port 8080 on
  this machine by default, and set to trust Caddy (`172.31.80.10`, or the `172.31.80.0/24`
  network) for `X-Forwarded-For`. In 5th Echelon Enhanced that's `[public] proxies`, and
  `require_tls_for_credentials` keeps its passwords on HTTPS. Don't run its own Caddy as well;
- in `docker/.env`: `CROSSCOM_DOMAIN`, `OTHER_DOMAIN` (the other server's domain) and, if it
  isn't on port 8080 of this machine, `OTHER_UPSTREAM` (`host:port`, as Caddy reaches it).

Then start this server with all three files:

```sh
docker compose -f docker/compose.yaml -f docker/compose.https.yaml -f docker/compose.shared.yaml up -d
```

Players of this game set the server up by its domain, not the machine's address: the game finds
its online config either way, but the launcher and the overlay ask by name, and an address alone
would reach the other server's API (the launcher and the overlay say so when it does). Caddy's
rules are in `docker/Caddyfile.shared`.

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
# trusted_proxies = ["127.0.0.1"]   # a reverse proxy on this machine, giving HTTPS
# accept_logs = true                 # players' logs (see below)
# logs_dir = "logs"
# logs_max_mb = 2048
# logs_keep_days = 30
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

Without [HTTPS](#https-with-caddy), sign-up goes over plain HTTP and passwords cross the network
unencrypted; the launcher only does that with a server set up by a private IP address. With it, use
`https://DOMAIN/accounts`. Either way, players should use a password they use nowhere else. If you leave sign-up
open without a code, anyone who can reach the server can make accounts. The server refuses a code
shorter than 12 characters; make it random (`openssl rand -base64 12`), not a word.

## The launcher's API

Always on, on port 80 next to the online config. The launcher uses it; so can a website or a bot.

`GET /api/info` gives the server's version, how many players are signed in to it, and the news
(as the game shows it: the first 14 items, cut the same way; the date is today's for an item
without one):

```json
{"name": "Cross-Com Enhanced", "game": "ghost-recon-future-soldier", "version": "0.1.0", "players_online": 3, "news": [
  {"title": "Double XP weekend", "body": "All weekend, every ranked match.", "date": "2026-11-20", "shows": "multiplayer"}
]}
```

`game` says which game the server is for: the launcher and the overlay check it, and say so
when an address reaches another game's server (on a machine that hosts both, see above).
`shows` is `null`, `"multiplayer"`, `"campaign"` or `"guerrilla"`. A player counts from their
sign-in until their game disconnects, or a minute after it stops answering.

`POST /api/login` with `{"name": "Kozak", "password": "their-password"}` checks an account:
`200 {"pid": ...}` when it matches, `401 {"error": "wrong name or password"}` when it doesn't.
Failed tries count with the game's own sign-in: after 10 in 15 minutes from one address, or for
one name, it answers `429`. Like sign-up, it goes over HTTPS when the server has it; through a
trusted proxy, both answer `403` to a request that didn't come over HTTPS.

`GET /api/leaderboard?mode=conflict` ranks every player who has played that mode (`all`,
`conflict`, `decoy`, `saboteur`, `siege`, `stockade` or `takeover`; `all` without one), from
the numbers the game saves after each match: most wins first, then kills, then objectives, at
most 100. The in-game overlay shows it.

```json
{"mode": "conflict", "modes": ["all", "conflict", ...], "rows": [
  {"rank": 1, "name": "Kozak", "wins": 3, "losses": 1, "kills": 9, "deaths": 2, "objectives": 1, "seconds": 1140}
]}
```

## Players' logs

After each game, the player's client sends its log (`crosscom.log`, what the client DLLs did:
signing in, the overlay, anything that failed) to the server, so when something breaks you can
read what happened on their PC without asking them for files.

**What and when.** Each run of the game has its own log: as the game starts, the last run's
`crosscom.log` moves to `logs\crosscom-<date>-<time>.log` in the game's folder (the 20 newest are
kept). The launcher sends the run that just ended as soon as a game it started exits (with the
game's exit code), and any others still waiting as it starts. A game started without the launcher
sends the earlier runs itself, over plain HTTP, a little after it signs in. Each run is sent once;
it's then renamed `*.sent.log`, and the player keeps it.

**What's taken out first.** Before a log leaves the PC, the client takes out the Windows user
name (wherever it appears, and the folder name in `C:\Users\<name>`) and the account's password,
should either be in it; both become `<user>` and `<password>`. The account name stays: the server
knows it. A log over 2 MiB is cut to its first 256 KiB and its last 1.75 MiB. The copies on the
player's PC aren't changed.

**Who may send.** `POST /api/logs` is only taken from an address the account it names
(`X-Crosscom-Account`) signed in to the game from in the last 24 hours (IPv4 exactly, IPv6 by its
/64; behind Caddy, the player's own address), otherwise `403`. It isn't a password check because
the game's own DLL sends logs over plain HTTP, where a password would cross the network
unencrypted; where the player plays from stands in for it, so strangers can't fill your disk. On
top of that: 20 an hour from one address (`429`), 4 MiB at most (`413`, refused from the
`Content-Length` before the body is read), 4 at once. The same run sent twice gets the same report.

**Where they're kept.** In `logs_dir` (`logs` beside the database: `/srv/crosscom/logs` in the
Docker volume), a folder a day: `<YYYY-MM-DD>/<report id>-<account>.log`, with a `.json` beside it
holding what the client said (account, client and launcher versions, DX11 or DX9, Windows
version, the run's file name, the exit code), the client's address and when it came. A report ID
is the time it came (UTC) and six random hex digits: `20261115-123456-ab12cd`; the launcher shows
the player the last one in its Settings, so they can tell you which is theirs. Logs older than
`logs_keep_days` (30), and the oldest past `logs_max_mb` (2048) in all, are deleted as new ones
come. They're never served over HTTP.

**Reading them:**

```sh
docker compose -f docker/compose.yaml exec server /app/crosscom-server \
  --config /srv/crosscom/crosscom-server.toml logs list          # newest first
docker compose -f docker/compose.yaml exec server /app/crosscom-server \
  --config /srv/crosscom/crosscom-server.toml logs show 20261115-123456-ab12cd
```

(Without Docker: `crosscom-server --config crosscom-server.toml logs list`.) The files are plain
text in the volume too.

**Turning it off.** `accept_logs = false` (`CROSSCOM_ACCEPT_LOGS=false`) and the server refuses
them all (`403`). Players turn sending off in the launcher's Settings ("Send game logs to the
server after each game") or with `send_logs = false` in `crosscom.toml`.

**Tell your players.** Something like: "After each game, your Cross-Com client sends its log to
this server so we can fix problems. It says what the client did (signing in, errors), your
account name, your Windows version and your IP address; your Windows user name and your password
are taken out first. Logs are kept for 30 days and only the server's hosts read them. Turn it off
in the launcher's Settings."

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
