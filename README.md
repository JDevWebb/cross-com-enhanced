<p align="center">
  <img src="./docs/logo.svg" width="140" alt="Cross-Com Enhanced">
</p>

<h1 align="center">Cross-Com Enhanced</h1>

<p align="center">
  <b>Community servers for <i>Tom Clancy's Ghost Recon: Future Soldier</i> multiplayer.</b><br>
  Conflict, Guerrilla and co-op, years after the official servers went dark.
</p>

---

**Status: early development.** Nothing is playable yet. This repository will hold the server, the client DLLs the game loads to reach it, and a launcher that sets the game up for you.

## How it works

Future Soldier's PC version talks to Ubisoft's Quazal-based online services, the same family *Splinter Cell: Blacklist* uses. Cross-Com Enhanced answers in their place:

- **The server** speaks the game's online protocols: sign-in, profiles, matchmaking, friends and NAT traversal.
- **The client DLLs** replace the game's platform libraries, so the game signs in to a Cross-Com server instead of Ubisoft's.
- **The launcher** finds the game, installs the DLLs and creates your account.

Matches themselves are still hosted by the players, as they always were.

## Playing

Put `crosscom-launcher.exe` and the two client DLLs in one folder and run the launcher. It finds the game (Ubisoft Connect or Steam installs, or a folder you choose), checks the server, makes your account if the server allows sign-up, installs the client (keeping the game's own files, which *Restore the game's files* puts back) and starts the game.

To run a server for your group, see [docs/hosting.md](docs/hosting.md).

## Standing on the shoulders of

Cross-Com Enhanced is developed by [JDevWebb](https://github.com/JDevWebb), as a sister project to [5th Echelon Enhanced](https://github.com/JDevWebb/5th-echelon-enhanced) for Splinter Cell: Blacklist.

- **[unixoide](https://github.com/unixoide)** wrote [5th Echelon](https://github.com/unixoide/5th-echelon) and its Quazal implementation (PRUDP, RMC, Kerberos tickets), which `quazal/` is built on, with its history and authors kept. Also by its contributors Matthias Walther, koteykaby and Askorbinovaya Kislota.
- **[Phoenix Network](https://github.com/Phoenix-Network-Ltd/Ghost-Recon-Future-Soldier-Research)** (Michał Kapała): the first Future Soldier backend research, which worked out much of the sign-in and recorded what the official servers answered, such as the players' privileges.
- **[zeroKilo](https://github.com/zeroKilo)** and **[Michał Kapała](https://github.com/michal-kapala)**: [GROBackendWV](https://github.com/zeroKilo/GROBackendWV), a Ghost Recon Online backend on the same online stack.
- **[Kinnay](https://github.com/Kinnay)**: the [PRUDP documentation](https://github.com/Kinnay/NintendoClients/wiki/PRUDP-Protocol).

## Licence

Cross-Com Enhanced's own work is under the MIT licence. The code from upstream 5th Echelon isn't licensed by its authors yet. [LICENSE.md](LICENSE.md) says exactly which is which.

Ghost Recon: Future Soldier is a trademark of Ubisoft Entertainment. This project isn't affiliated with Ubisoft and contains no game files: you need your own copy of the game.
