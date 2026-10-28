# Future Soldier's online protocol

What Cross-Com Enhanced has worked out about how *Ghost Recon: Future Soldier* (PC) talks to its online services. These are facts about the game, written in our own words; no game data is copied here.

The game uses Ubisoft's online SDK, built on Quazal Rendez-Vous: the same family as *Splinter Cell: Blacklist* (see [5th Echelon](https://github.com/unixoide/5th-echelon)) and *Ghost Recon Online* (see [GROBackendWV](https://github.com/zeroKilo/GROBackendWV)). Many details below were first found by the [Phoenix Network GRFS backend](https://github.com/Phoenix-Network-Ltd/Ghost-Recon-Future-Soldier-Research) and confirmed against the game.

**Status (2026-10-29):** worked out from the game and the projects above, not yet tested against the game. Items marked *to confirm* are settled when the game first calls our server: it logs every call it can't answer.

## 1. Finding the server

1. The game asks `onlineconfigservice.ubi.com` over HTTP: `GET /OnlineConfigService.svc/GetOnlineConfig?onlineConfigID=23ad683803a0457cabce83f905811dbc`.
2. The answer is a JSON array of `{"Name": ..., "Values": [...]}` entries. The one that matters is `SandboxUrl`, the auth server's station URL (`prudp:/address=<host>;port=<port>`), with `SandboxUrlWS` as `<host>:<port>`. The game also asks for six `uplay_*` web addresses, which can point at the same host.
3. Cross-Com's client sends the `onlineconfigservice.ubi.com` lookup to the Cross-Com server, as 5th Echelon does for Blacklist.

## 2. Transport

- **PRUDP v0** over UDP, as in 5th Echelon's `quazal` crate.
- **Access key `8dtRv2oj`** (Ghost Recon Online uses the same one). Packet signatures are HMAC-MD5 keyed with MD5 of the access key; the checksum seed for secure (RVSec) streams is the byte sum of the key, `0xE3`.
- Payloads are RC4-encrypted with the default Quazal key `CD&ML`.
- Phoenix used UDP 21030 for the auth server and 21032 for the secure server; any ports work, since the online config and the login answer name them.

## 3. Signing in

1. The game connects to the **auth server** and calls `TicketGranting.LoginEx` (protocol 10, method 2). Its extra data is a `UbiAuthenticationLoginCustomData { userName, onlineKey, password }`: the Ubisoft account name, the game's CD key and the password, which the game gets from the Orbit library (section 6).
2. A built-in tracking user signs in with plain `TicketGranting.Login` (method 1) as `Tracking`.
3. The server answers with the player's PID, a Kerberos ticket, and `RVConnectionData`, whose station URL is the **secure server**.
4. `TicketGranting.RequestTicket` (method 3) issues the ticket for the secure server. The ticket key is MD5 applied `65000 + (pid % 1024)` times to the password (`UbiDummyPwd` for players), as in `quazal/src/kerberos.rs`.
5. On the secure server the game calls `SecureConnection.RegisterEx` (protocol 11, method 4) with its station URLs; the server answers with a connection ID and the player's public URL.

## 4. Protocols

The game declares 27 protocols with full method signatures; methods are numbered from 1 in declaration order. The IDs are in [`data/protocol_ids.json`](../data/protocol_ids.json), which the protocol code generator reads. Sources: **B** 5th Echelon's Blacklist mapping (same SDK), **P** Phoenix, **Q** standard Quazal numbering.

| ID | Protocol | Methods | Source | Needed for |
|---:|---|---:|---|---|
| 3 | NATTraversal | | B, P | matches over the internet |
| 10 | TicketGranting | 6 | B, P | sign-in |
| 11 | SecureConnection | 4 | B, P | sign-in |
| 14 | Notification | | B | events pushed to the game |
| 18, 19 | Health, Monitoring | | B | |
| 20 | Friends | | B, P | friends |
| 21 | MatchMaking | 41 | P | lobbies (Gatherings) |
| 24 | PersistentStore | 9 | P | probably profiles and progression |
| 25 | AccountManagement | | B, P | |
| 29 | UbiAccountManagement | | B | |
| 31 | News | | P | |
| 33 | UbiNews | | P | |
| 35 | Privileges | 6 | B, P | main menu (`GetPrivilegesEx`, 6) |
| 36 | Tracking (v3) | | B, P | main menu (configuration) |
| 39 | Localization | | B, P | |
| 42 | GameSession | 26 | B, P | sessions and invitations |
| 47 | UbiFriends | | P | friends |
| 50 | MatchMakingExt | 6 | Q, *to confirm* | lobbies |
| 51 | TitleStorage | 4 | P | content files (`BrowseContentsByType` 2, `DownloadContent` 4) |
| 86 | SearchFriends | | P | |
| 87 | FriendMessagesExtension | | P | |
| 5001–5008 | Rpne, OverlordNews, OverlordCore, ExtraContent, OverlordFriends, Awards, OverlordChallenge, OverlordDare | | B, P | main menu (`OverlordCore.GetSettings`, 1) |

**To confirm:**
- **UplayWin** is 49 in Blacklist and 120 in Phoenix (which came from Ghost Recon Online).
- **TrackingExtension** is 1001 in Blacklist and 125 in Phoenix.
- Phoenix also answers protocols 109, 110, 111 and 126 under Ghost Recon Online's names (rich presence, clans, misc settings, events tracking). Which Future Soldier protocols they are is to confirm.
- Admin protocols the game declares but shouldn't call: GameSessionAdmin, TitleStorageAdmin, NewsAdmin, LocalizationAdmin, RemoteLogDevice.

## 5. Matchmaking

Lobbies use classic Quazal **Gatherings** (MatchMaking, 21): `RegisterGathering`, `FindByType`/`FindBySQLQuery`, `Participate`, `LaunchSession`, `GetParticipantsURLs` and the rest. The match itself is hosted by a player and runs peer to peer; the server introduces the players (NAT traversal) and can relay them.

## 6. Platform libraries the game loads

The game loads two Ubisoft libraries by name, and calls only these functions:

**`ubiorbitapi_r2_loader.dll`** (Orbit; C++ `mg::orbitclient`, `thiscall`): `OrbitClient` constructor, `GetInstallationErrorNum`, `GetInstallationErrorString`, `StartLauncher`, `Update`, `GetLoginDetails`, `GetRequestUniqueId`, `GetSavegameReader`, `GetSavegameWriter`; `SavegameReader::Read`, `Close`; `SavegameWriter::SetName`, `Write`, `Close`. `GetLoginDetails` gives the game the account name, password and CD key it signs in with; the savegame calls are its cloud saves. Results come back through listener objects the game passes in.

**`uplay_r1_loader.dll`**: `UPLAY_Init`, `UPLAY_Quit`, `UPLAY_Update`, `UPLAY_GetNextEvent`, `UPLAY_FRIENDS_Init`, `UPLAY_FRIENDS_GetFriendList`.

Cross-Com's client replaces both.

## 7. Still to work out

- The listener interfaces Orbit calls back on (their method order).
- What the game needs to run without PunkBuster, whose servers are gone.
- The protocol IDs marked *to confirm*.
