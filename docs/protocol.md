# Future Soldier's online protocol

What Cross-Com Enhanced has worked out about how *Ghost Recon: Future Soldier* (PC) talks to its online services. These are facts about the game, written in our own words; no game data is copied here.

The game uses Ubisoft's online SDK, built on Quazal Rendez-Vous: the same family as *Splinter Cell: Blacklist* (see [5th Echelon](https://github.com/unixoide/5th-echelon)) and *Ghost Recon Online* (see [GROBackendWV](https://github.com/zeroKilo/GROBackendWV)). Many details below were first found by the [Phoenix Network GRFS backend](https://github.com/Phoenix-Network-Ltd/Ghost-Recon-Future-Soldier-Research) and confirmed against the game.

**Status (2026-10-31):** the game signs in, reaches its menus, hosts matches and plays them against a Cross-Com server. Sections marked *not yet seen in a game* were worked out from the game's own code and tested with the testbot only.

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

The game declares 27 protocols with full method signatures; methods are numbered from 1 in declaration order. The IDs are in [`data/protocol_ids.json`](../data/protocol_ids.json), which the protocol code generator reads. It also has protocols of its own that aren't among those declarations (108 and up), and the Overlord ones (5001-5008), which come from a shared Ubisoft library.

| ID | Protocol | What the game uses it for |
|---:|---|---|
| 3 | NATTraversal | being probed by players it wants to reach (section 7) |
| 10 | TicketGranting | sign-in |
| 11 | SecureConnection | sign-in (`RegisterEx`) |
| 14 | Notification | events the server pushes (section 8) |
| 20 | Friends | the friends list, requests, blocking (section 8) |
| 35 | Privileges | what the player may do, including owning each DLC pack (9001-9003; 3000, 3010 and 3020 are only trials) |
| 36 | Tracking (v3) | its tracking configuration and tags |
| 39 | Localization | the locale |
| 42 | GameSession | parties, matches and invitations (section 6) |
| 47 | UbiFriends | a second friends list, added to Friends' |
| 51 | TitleStorage | content files |
| 86 | SearchFriends | finding players by name |
| 87 | FriendMessagesExtension | the message sent with a friend request |
| 108 | PlayerStatistics | rank, experience and the profile (section 5) |
| 109 | RichPresence | presence |
| 110 | Clans | the player's clan (13), and a list asked as the menus load (28: a list and a u32) |
| 111 | Misc | the server time (1), generic settings (3) |
| 120 | UplayWin | Uplay actions and rewards |
| 121 | MapList | which maps a match may vote between (section 6) |
| 125 | TrackingExtension | |
| 5002 | OverlordNews | news: `GetNews(predicate, result range)` answers a list |
| 5003 | OverlordCore | settings |
| 5008 | OverlordDare | `RequestDareInfo(u32)` answers one dare-info object (its fields aren't known) |

Every call the server can't answer is logged with its parameters, which is how the rest are worked out.

## 5. Player statistics (108)

Methods 1-5 send and 6-10 read values of one type each: i32, i64, f32, string and blob (a string or blob is a u16 length, then the bytes).

```text
send:  list<{ board: u8, keys: list<u32>, values: list<{ column: u8, value, op: u8 }> }>
read:  list<{ board: u8, keys: list<u32>, columns: list<u8> }>, players: list<u32>
reply: list<{ player: u32, rows: list<{ board: u8, keys: list<u32>, values: list<{ column: u8, value }> }> }>
```

The game sends its whole profile from the menus (350 rows of board 3, keyed by pairs), every op 2 (set). A read's key `-1` matches any key, so a board read with `-1` keys answers a row for each stored key set.

## 6. Sessions, matchmaking and maps

Parties and matches are **GameSession** sessions (42), not Gatherings. The match itself is hosted by a player's game, peer to peer.

- **Properties** the game sets: 3 public slots, 4 private (invite-only) slots, 7 whether the session wasn't matchmade (1 for a party or a custom match), and 100-117 the match's settings.
- **Quick match** searches (`SearchSessions`, query 2) with the public slots it needs (property 3) and the settings it wants; when it finds nothing it creates a match with the same settings and waits in its lobby. A match is offered when it's matchmade, has that many free public slots and the same settings.
- **Station URLs**: the game registers its peer-to-peer address with `RegisterURLs` (21) as `udp:/address=...;port=...;PID=...;RVCID=...`, learning its public address and port from the server's echo (PRUDP user packets on port 1: the server answers with the packet and `udp:/address=IP;port=PORT`).
- **Map list (121)**: `GetAvailableMapCategories()` and `GetMapList(u32 category)` both answer a `list<u32>` of world keys. The category has a bit per game mode, `1 << (n + 2)` in the menu's numbering (2 Conflict, 3 Decoy, 4 Saboteur, 5 Siege, 6 Stockade, 7 Takeover, 8 Guerrilla), a bit per DLC pack the lobby's players have (`0x4000` the base game, `0x8000` Arctic Strike, `0x10000` Khyber Strike; the game leaves `0x4000` out unless the lobby has both packs or neither; Raven Strike adds no adversarial content and has no bit), and the match size in the low bits (1 for 8 players, 2 for 12). Each map has one world per mode, and the world loaded decides the mode. Stockade needs Arctic Strike and Takeover Khyber Strike on every map (the game refuses them otherwise, and their worlds are in those packs); the two night maps are the base game's. The world keys, and which pack each needs, are in [`server/crosscom-server/src/maps.rs`](../server/crosscom-server/src/maps.rs).
- **DLC packs in matchmaking**: the game doesn't tell the server which packs a searching player has, so Cross-Com's client adds them to the CD key it signs in with, as `;packs=` and the pack numbers (`;packs=13`: Arctic and Khyber Strike). A lobby's packs are the ones its host's map list asks for. A search offers a player only lobbies whose packs, and whose mode's pack (Stockade: Arctic Strike, Takeover: Khyber Strike), it has; a client that doesn't say is offered everything, and the game's own check refuses what it can't load as it joins.
- **The server time** (`Misc.1`) is asked right after a match's map list: an 8-byte Quazal date-time (seconds in bits 0-5, minutes 6-11, hours 12-16, day 17-21, month 22-25, year from 26).

## 7. NAT traversal

A game that wants to reach other players calls `RequestProbeInitiation` (3.1, with their station URLs) or `RequestProbeInitiationExt` (3.3, with the station to probe too). The server calls `InitiateProbe` (3.2) on each of those players' games, so their packets open the way through both routers. A call the server makes to a game must be numbered and signed as the server's own packets are; one made like a client's request is dropped.

## 8. Friends, events and invitations (*not yet seen in a game*)

- **Relationships**, as the game's friend list reads them: 1 a friend, 2 a friend request, 3 blocked; a status of 1 is online. The list is built from `Friends.GetRelationships`, then `UbiFriends.GetRelationshipsList`, then the blocked players (`Friends.GetDetailedList(3)`).
- **Events** (`ProcessNotificationEvent`, 14.1): `{ source pid, type, param1, param2, string, param3 }`. The type is a category times 1000 plus a subtype, and the game dispatches on the category: 1 friend events (param1 the other player, param2 the relationship: 2 "asks to be friends", 1 "now friends"), 3 participation events, 7 game session events (7005 an invitation received: param1 the session type, param2 its ID, the string the message), 1000 and up custom events.
- **Game invitations** are GameSession methods 12-19: send, list and count received and sent, accept, decline, cancel. After accepting, the game joins the session itself.

## 9. Platform libraries the game loads

The game loads two Ubisoft libraries by name, and calls only these functions:

**`ubiorbitapi_r2_loader.dll`** (Orbit; C++ `mg::orbitclient`, `thiscall`): `OrbitClient` constructor, `GetInstallationErrorNum`, `GetInstallationErrorString`, `StartLauncher`, `Update`, `GetLoginDetails`, `GetRequestUniqueId`, `GetSavegameReader`, `GetSavegameWriter`; `SavegameReader::Read`, `Close`; `SavegameWriter::SetName`, `Write`, `Close`. `GetLoginDetails` gives the game the account name, password and CD key it signs in with, through the listener the game passes in: its first virtual method takes `(request id, user, password, CD key)`, each copied into an 80-byte buffer (so each is at most 79 characters).

**`uplay_r1_loader.dll`**: `UPLAY_Init`, `UPLAY_Quit`, `UPLAY_Update`, `UPLAY_GetNextEvent`, `UPLAY_FRIENDS_Init`, `UPLAY_FRIENDS_GetFriendList`; the game runs with all of them answering 0.

Cross-Com's client replaces both. The game plays its videos with Bink; the client can skip the three splash videos by answering `BinkOpen` for them as for a missing file.

## 10. Still to work out

- Whether Guerrilla's maps are chosen through the map list at all (Raven Strike's Sawmill can't be asked for there).
- The dare-info object (5008.1), and what Clans method 28 is.
- Statistics ops other than 2 (set), if the game sends any.
- What the game needs to run without PunkBuster, whose servers are gone (so far: nothing).
