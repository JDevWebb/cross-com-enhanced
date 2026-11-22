# Security policy

Thank you for helping keep Cross-Com Enhanced and its players safe. If you find a weakness, please tell us privately first, and give us a chance to fix it before anyone else hears about it.

## Reporting a vulnerability

**Please don't open a public issue, discussion or pull request for a security problem.**

Report it privately on GitHub instead: on this repository's **Security** tab, choose **[Report a vulnerability](https://github.com/JDevWebb/cross-com-enhanced/security/advisories/new)**. Only the maintainer sees it, and we can talk it through there and credit you in the advisory.

Helpful to include:
- what's affected (the launcher, the client DLLs, the server, or a community server) and which version (the launcher's About page; `/api/info` on a server);
- the steps to reproduce it, and the smallest proof of concept you can;
- what an attacker could do with it, and anything you know about who's exposed;
- whether you'd like credit, and under what name.

## What happens next

- **Within 7 days:** we acknowledge your report.
- **Within 14 days:** we tell you whether we've confirmed it, and how serious we think it is.
- **Then:** we work on a fix and keep you posted. Serious problems that affect the community servers or players' accounts and PCs come first.
- **Disclosure:** we publish an advisory when the fix is released, crediting you unless you'd rather not be named. We aim to fix within 90 days; if that isn't possible, we'll agree a timeline with you. Please don't disclose before then without talking to us.

Cross-Com Enhanced is a free project run by one person, with no bug bounty. What we can offer is a prompt, honest response and credit for your work.

## What's in scope

- **This repository's code:** the launcher (`crosscom-launcher.exe`, including the Windows Firewall rules it adds and its connection test), the client DLLs (`uplay_r1_loader.dll` and `ubiorbitapi_r2_loader.dll`) with their in-game overlay and the router port forwards they ask for, and the server (`crosscom-server`: the game's online services, and its HTTP API for accounts, sign-up, news, leaderboards, log uploads and router reports), with its Docker and Caddy setup.
- **The community test network:** `grfs-test.jdevwebb.net`.
- **The release process:** the release workflow, `scripts/release.sh`, `sign-release.sh` and `verify-release.sh`, and anything that would let a release or the server's image be published or pass the checks without the release key.
- **Especially interesting:** anything that lets someone take over another player's account or identity, run code on a player's PC or a server, read or change another player's data (friends, squads, invitations, challenges, statistics, uploaded logs), use the server to make players' games send traffic to other hosts (the NAT probes), or get a player's password or address when it shouldn't be shared.

## Out of scope

- *Tom Clancy's Ghost Recon: Future Soldier* itself, and Ubisoft's services. Report those to Ubisoft.
- Problems that exist only in [upstream 5th Echelon](https://github.com/unixoide/5th-echelon), in code this project doesn't change. Please report those upstream; if they affect this project too, we'd like to hear as well.
- Servers run by other people with this software: contact their operators. If the problem is in this software, report it here.
- What the game itself sends between players in a match: the game connects players directly, as it always did, so they see each other's addresses.
- Reports with no security impact, such as missing headers with no exploit, version disclosure, or results from an automated scanner without a demonstrated issue.
- Antivirus programs flagging the launcher or the client DLLs: see [Antivirus warnings](README.md#antivirus-warnings).

## Testing the community servers in good faith

We won't pursue or complain about research that follows these rules, and we'll treat it as authorised:
- **Use your own accounts,** and only touch data that's yours. Don't access, change or delete other players' accounts, friends, messages or data; if you come across any, stop and tell us.
- **Don't disrupt the service:** no denial-of-service or volumetric testing, flooding, spam, or tests that degrade play for others. A server of your own is the right place for anything heavy: `scripts/dev-server.sh` runs one locally, and [docs/hosting.md](docs/hosting.md) sets one up.
- **Keep it minimal:** use the smallest proof of concept that shows the problem, and don't keep any data you obtain.
- **No social engineering** of players or the maintainer, and no physical attacks or attacks on hosting providers.
- **Report it promptly,** and keep it private until it's fixed.

If you're unsure whether something is OK, ask in your private report first.

## Supported versions

Security fixes go into the latest release only; older versions aren't patched separately. The community test network runs the latest server. Releases are published only on this repository's [releases](https://github.com/JDevWebb/cross-com-enhanced/releases) page: each is built by its release workflow, with build provenance, and signed with a release key that never meets the build ([Releases](docs/releasing.md)). The files aren't code-signed for Windows yet, so get them only from there, and check them. If you run your own server, keep it updated.
