# Hosting a Cross-Com server

The server is one program with three ports. Run it on a machine your players can reach: a Linux
PC at home with the ports forwarded, a cheap VPS, or a machine on the LAN or VPN you play over.

| Port | Protocol | What for |
|---|---|---|
| 80 | TCP | The online config the game asks for first (it always uses port 80); the API, on a LAN or VPN server without HTTPS |
| 443 | TCP | [HTTPS](#https-with-caddy), which a server on the internet needs: the launcher's and the client's API calls (sign-in, sign-up, logs, router reports, the overlay's reads) |
| 21030 | UDP | Sign-in |
| 21031 | UDP | Everything else: profiles, matchmaking, friends |

Matches themselves are hosted by a player's game, peer to peer; the server only introduces the
players, and has their games punch through their routers. The client asks each player's router
to forward the game's port too, when it can (see [Connection records](#connection-records)).

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

The launcher and the client DLL use the server's API over HTTPS: passwords when players sign in
or make an account, players' logs, the client's router reports, and the overlay's reads. They
fall back to plain HTTP only for a server set up by a private IP address (`10.x`, `172.16-31.x`,
`192.168.x`, `100.64-127.x` as Tailscale uses, `fc00::/7`, link-local, or this machine) where
nothing answers on port 443 at all; the launcher tells the player their password went
unencrypted. A server set up by name or by a public address is only spoken to over HTTPS,
whatever its name points at, and so is one whose HTTPS answers but fails (a certificate that
isn't valid): they refuse and say the server needs HTTPS. So a server on the internet needs
HTTPS (a domain and Caddy, below), and a LAN or VPN server without a domain is set up in the
launcher by its IP address.

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

Use the same two `-f` files for every other compose command (`logs`, `exec`, `down`). Players set
up the launcher with the domain: its certificate is for that name.

What stays on plain HTTP, on port 80, and why:

- `GET /OnlineConfigService.svc/GetOnlineConfig`: the game asks for it there and can't use HTTPS.
- `GET /api/...` (the server's info, news and other read-only answers), for websites and tools
  that read them; Cross-Com's own launcher and overlay read them over HTTPS. None carries a
  password.

Everything else on port 80, notably `POST /accounts`, `POST /api/login`, `POST /api/logs` and the
router reports (`POST /api/forward`, `/api/reach`), is redirected to
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
], "udp_ports": [21030, 21031]}
```

`game` says which game the server is for: the launcher and the overlay check it, and say so
when an address reaches another game's server (on a machine that hosts both, see above).
`shows` is `null`, `"multiplayer"`, `"campaign"` or `"guerrilla"`. A player counts from their
sign-in until their game disconnects, or a minute after it stops answering. `udp_ports` are the
sign-in ports, which clients' connection tests ask (see [Connection records](#connection-records)).

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
sends the earlier runs itself, over HTTPS, a little after it signs in. Each run is sent once;
it's then renamed `*.sent.log`, and the player keeps it.

**What's taken out first.** Before a log leaves the PC, the client takes out the Windows user
name (wherever it appears, and the folder name in `C:\Users\<name>`) and the account's password,
should either be in it; both become `<user>` and `<password>`. The account name stays: the server
knows it. A log over 2 MiB is cut to its first 256 KiB and its last 1.75 MiB. The copies on the
player's PC aren't changed.

**Who may send.** `POST /api/logs` is only taken from an address the account it names
(`X-Crosscom-Account`) signed in to the game from in the last 24 hours (IPv4 exactly, IPv6 by its
/64; behind Caddy, the player's own address), otherwise `403`. It isn't a password check: the
game's own DLL sends logs, and it doesn't hold the player's password for it (the game has it);
where the player plays from stands in for one, so strangers can't fill your disk. Through Caddy
it's only taken over HTTPS (`403` otherwise). On
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

## Connection records

Players' games connect to each other, and the game doesn't tell the server when they can't: a
player who can't reach a match's host leaves the session after half a minute or so, and their
game says "Connection to server lost". So the server records how each join went, to show how
often that happens and with which routers.

**What's recorded.** A join is noted when a player joins a session (or its host adds them), with
the NAT probes they ask for meanwhile, and recorded when it ends: how long they stayed, and how it
ended (they left, the host took them out, their game lost the server, or the host had gone
first). A join that ends within 90 seconds is counted as ending early: likely a failed
connection, though a player backing out of a lobby looks the same. A join can also fail before
it starts: a player picks a lobby from a search, can't reach its host, and gives up (their game
then hosts a lobby of its own). A lobby picked whose host hasn't added the player within 60
seconds is recorded as `unreached`. Each record also has what both
players' clients said about their routers: whether they forward the game's port, and how they map
ports (consistent, or symmetric: a port per destination, which other players mostly can't punch
through). Records name players by account, with no addresses, and are kept in the database for
90 days. A join that ends early is also logged (`... ended after 33 s: left`).

**Reading them:**

```sh
docker compose -f docker/compose.yaml exec server /app/crosscom-server \
  --config /srv/crosscom/crosscom-server.toml connections 30     # the last 30 days
```

```text
Joins in the last 30 days: 120
  failed or ended within 90 s: 9 (8%)
    never got in: the host never added them: 3
    the player left: 3
    the host took them out: 1
    their game lost the server: 2
  (and 4 ended because the host had gone, not counted)

By the joining player's router                  joins  ended early
  port forwarded                                    51      1 (  2%)
  no forward, consistent NAT                        58      4 (  7%)
  no forward, symmetric NAT                         11      4 ( 36%)

By both players' routers                        joins  ended early
  either forwarded                                  70      2 (  3%)
  ...

Players whose joins ended early most often:
  Kozak                    3 of 5
```

A player near the top of that last list, with a symmetric NAT and no forward, is one to ask to
turn on UPnP, or to run the launcher's connection test (Settings › Connection). In testing, a
player behind a symmetric NAT couldn't get into a lobby whose host had no forward either
(`unreached`), and got in as soon as the host's router forwarded.

**Whether players can be reached where they announce themselves.** When a game registers its
addresses, or its client reports a new forward (not the same one renewed), the server checks the address other players will be
given first: it sends a NAT probe there from a socket of its own, one the player's router has
never seen, and waits a second and a half for the game to answer, as games answer each other's.
An answer means the address takes packets from anyone, so any player can join that game's
matches (`... answers at 203.0.113.7:2346 from anywhere`); none means only players whose games
can punch through can (`... didn't answer ... from outside`). Records note it for both players,
and the summary sums joins up by the host's (`By the host's announced address`). A router that
lets in anything from an address its player has already sent to, whatever the port, counts as
reachable here, as the server's address is one it has.

**Matchmaking around it.** A player whose router gives a port per destination (a symmetric NAT,
as their client reports) and doesn't forward the game's port can't punch through to a host. Their
Quick Match searches aren't offered lobbies whose host was checked and can't be reached from
outside (logged: `... can't punch through ...: not offered 1 lobby(ies) ...`), so their game
hosts a lobby of its own instead of trying a join that would fail. Hosts not checked are offered
as before, and so is every lobby to everyone else; invitations and joining a friend aren't
touched.

**When a game announces the wrong adapter.** A PC with a VPN, Hyper-V or VirtualBox may have its
game announce that adapter's address on its network instead of the one that reaches the server.
The client's router report names the right one: the server then puts it first for players at the
same house, and logs `... announced another adapter's address than the one reaching the server`.
In testing, the game picked its peer-to-peer address well: on a PC whose VPN adapter (Tailscale)
had a lower interface metric than its Wi-Fi, it announced the Wi-Fi address for peer-to-peer and
listed the VPN's only for its connection to the server, which isn't given to other players.

**Where the router details come from.** While the game runs, the client reports the game's
peer-to-peer socket on the player's network to `POST /api/forward`, with the port the router
forwards to it, if any:

```json
{"account": "Kozak", "local": "192.168.1.20:2346", "forward": {"port": 40000, "how": "UPnP"}, "nat": "consistent"}
```

The server takes it as the named player's when their game is seen at the address it came from
(from a client that names no one, the player seen there whose game registered that socket)
(`200 {"address": ..., "port": ..., "open": ...}`: where it sees the caller, the socket's public
port once the game has registered it, and whether the server reached the game at its announced
address, once checked). A forward is then given to the other players in that player's matches
first, on the address the server sees the player at, for 35 minutes from each report (the client
renews it every 10). It isn't given to players on the same network: they get each other's
private addresses first.

`POST /api/reach` with `{"port": 40001, "nonce": "0123abcd..."}` sends `crosscom-reach ` and the
nonce, once, to that port at the caller's own address, which is how the client checks a forward
works. It only sends to the caller, to port 1024 or above, smaller than the request, and 4 a
minute from an address; reports are 12 a minute. Both take no password, and go over HTTPS:
through Caddy, the server refuses them over plain HTTP (`403`).

**Tell your players**, with the logs note: "While you play online, your client also tells this
server about your router (your PC's address on your network, the port it forwards to the game,
how it handles ports), and the server records how each join into a match went, by account and
without addresses, for 90 days. Turning off port forwarding in the launcher's Settings stops the
router report."

## Checking it works

`crosscom-testbot` signs in the way the game does and checks each step:

```sh
cargo run -p crosscom-testbot -- SERVER_ADDRESS NAME PASSWORD
cargo run -p crosscom-testbot -- SERVER_ADDRESS NEWNAME PASSWORD --signup CODE   # sign-up too
```

Two bots, or a bot and the game, test matchmaking: one runs with `--host 60`, the other with
`--join`. `--reach` checks what the client does about players' routers: the address echo on both
UDP ports, the router report and the reach packet (run it on the server's own machine, at
`127.0.0.1`: its reach packet goes to a socket it opens there).

## Upgrading

```sh
git pull
docker compose -f docker/compose.yaml up -d --build
```

Accounts and statistics stay in the volume.
