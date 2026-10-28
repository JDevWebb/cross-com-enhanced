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

## Standing on the shoulders of

Cross-Com Enhanced is developed by [JDevWebb](https://github.com/JDevWebb), as a sister project to [5th Echelon Enhanced](https://github.com/JDevWebb/5th-echelon-enhanced) for Splinter Cell: Blacklist.

- **[unixoide](https://github.com/unixoide)** wrote [5th Echelon](https://github.com/unixoide/5th-echelon) and its Quazal implementation (PRUDP, RMC, Kerberos tickets), which `quazal/` is built on, with its history and authors kept. Also by its contributors Matthias Walther, koteykaby and Askorbinovaya Kislota.
- **[zeroKilo](https://github.com/zeroKilo)** and **[Michał Kapała](https://github.com/michal-kapala)**: [GROBackendWV](https://github.com/zeroKilo/GROBackendWV), a Ghost Recon Online backend on the same online stack.
- **[Kinnay](https://github.com/Kinnay)**: the [PRUDP documentation](https://github.com/Kinnay/NintendoClients/wiki/PRUDP-Protocol).

## Licence

Cross-Com Enhanced's own work is under the MIT licence. The code from upstream 5th Echelon isn't licensed by its authors yet. [LICENSE.md](LICENSE.md) says exactly which is which.

Ghost Recon: Future Soldier is a trademark of Ubisoft Entertainment. This project isn't affiliated with Ubisoft and contains no game files: you need your own copy of the game.
