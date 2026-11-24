# Releases

Releases are built on GitHub Actions from a version tag, and signed by hand with a release key
that never meets the build. Every download comes with its checksum, the release key's signature,
its build provenance and its VirusTotal results.

## Checking a download

Every release has, next to its files:

| File | What it is |
|---|---|
| `SHA256SUMS` | Each file's SHA-256 |
| `SHA256SUMS.sig` | The release key's signature of the version and `SHA256SUMS` |
| `IMAGE`, `IMAGE.sig` | The server's Docker image, by digest, and the release key's signature of it |
| `THIRD-PARTY-NOTICES.txt` | The libraries the files are built with, and their licences |

**On Windows**, compare the file's hash with its line in `SHA256SUMS`:

```powershell
Get-FileHash .\crosscom-launcher.exe
```

**With OpenSSL 3** (Linux, macOS, or Git Bash on Windows), in a folder with the downloads and
`SHA256SUMS` and `SHA256SUMS.sig`, check the signature and every file at once with
[`scripts/verify-release.sh`](../scripts/verify-release.sh), which carries the release key:

```sh
scripts/verify-release.sh 0.1.0
```

**Build provenance:** with the [GitHub CLI](https://cli.github.com), this shows a file was built
by this repository's release workflow, from the release's tag:

```sh
gh attestation verify crosscom-launcher.exe --repo JDevWebb/cross-com-enhanced
```

**VirusTotal:** each release's notes link every file's report. The files aren't code-signed yet,
so some antivirus programs flag them by what they do (a launcher that installs DLLs into a game):
see [Antivirus warnings](../README.md#antivirus-warnings).

### The server image

`ghcr.io/jdevwebb/crosscom-server:<version>` is the server's image for a release. `IMAGE` names it
by digest, and `verify-release.sh` checks `IMAGE.sig`; pull it by that digest to get exactly the
image that was signed. [hosting.md](hosting.md) builds it from source instead.

### The release key

An Ed25519 key. Its private half stays off GitHub, with the maintainer; this is its public half,
which `scripts/verify-release.sh` carries too:

<!-- release-key -->
Not made yet: `scripts/sign-release.sh --keygen` makes it and writes it here.
<!-- /release-key -->

What it signs, for checking by hand: a file of the purpose line `cross-com/release/v1` (or
`cross-com/image/v1` for `IMAGE`), a newline, the version (`0.1.0`), a newline, then the signed
file exactly. The signature is base64, on one line. With the key above saved as `release.pem`:

```sh
{ printf 'cross-com/release/v1\n%s\n' 0.1.0; cat SHA256SUMS; } > signed
openssl base64 -d -A -in SHA256SUMS.sig > signature
openssl pkeyutl -verify -pubin -inkey release.pem -rawin -in signed -sigfile signature
```

## Making a release

`release.toml`'s `version` is the one release number for the launcher, the client DLLs and the
server: the launcher's About page and version details, the client's log and the logs it sends,
and the server's `/api/info` all show it. Between releases it names the next one, with a `-dev`
suffix on builds that aren't a release (`0.1.0-dev` until 0.1.0 is out); it moves on per release,
not per change.

Release notes go in `docs/releases/v<version>.md`. Their first line can name the release
(`# 0.2.0 "Name"`), which goes in its title; the workflow puts them first, then how to check the
download and the VirusTotal results, then GitHub's list of changes.

On the machine with the release key, one command does it all:

```sh
scripts/release.sh 0.1.0
```

It checks `main` is clean, that nothing private is in any commit (`scripts/check-clean.sh
--history`) and that CI passed on it, drops the suffix in `release.toml`, commits and tags
`v0.1.0`, pushes `main` and that tag, waits for the release workflow, runs `sign-release.sh`
(which asks once before signing), publishes, and moves `main` on to `0.1.1-dev`. CI doesn't run
again for the two version commits, or for a push that only changes release notes: the release
workflow builds the tag itself.

By hand, the same steps are:

1. Drop the suffix in `release.toml` (e.g. `0.1.0`) and commit it.
2. Tag it with exactly that version, and push that tag by name (never `--tags`):
   ```sh
   git tag v0.1.0 && git push origin main v0.1.0
   ```
3. When the release workflow has made the draft, sign and publish it:
   ```sh
   scripts/sign-release.sh v0.1.0
   ```

The release workflow (`.github/workflows/release.yml`):
- checks the tag matches `release.toml`;
- builds the client DLLs and the launcher with them inside on a Windows runner, the server on
  Linux, and `THIRD-PARTY-NOTICES.txt` (`scripts/notices.sh`, with cargo-about and `about.toml`);
- writes `SHA256SUMS` and attests each file's build provenance;
- scans the files with VirusTotal (`scripts/virustotal.sh`; needs the `VT_API_KEY` secret, a
  free VirusTotal account's key: without it, the notes link each file's report by its hash);
- makes the GitHub release as a draft, with the notes;
- pushes the server image to `ghcr.io`, tagged `:<version>-unsigned` only.

`sign-release.sh`:
- checks that the tag's commit is on `main`, every download against `SHA256SUMS`, that
  `SHA256SUMS` lists exactly the downloads, and each download's build provenance (when gh can),
  and prints the commit and hashes to compare with the workflow run;
- checks the `-unsigned` server image was built by the workflow from the tag, and writes its
  digest to `IMAGE`;
- signs the version and `SHA256SUMS` (and `IMAGE`) with OpenSSL, in a container with no network,
  a read-only filesystem and the key mounted read-only;
- checks the signatures with `verify-release.sh`, as everyone else checks them, uploads them and
  publishes the draft;
- tags the server image `:<version>` (and `:latest` for a release). That needs gh's token to have
  the `write:packages` scope (`gh auth refresh -s write:packages`); without it, it prints the
  commands.

A tag with a suffix (`v0.2.0-rc.1`) makes a pre-release, which isn't followed by a `-dev`.

### The first time

1. Make the release key, once, on the machine you'll release from (it needs Docker and the build
   image, `fes-build:local`), and back it up somewhere safe and offline:
   ```sh
   scripts/sign-release.sh --keygen
   ```
   It goes in `~/.config/cross-com-release/release.key`, and its public half into
   `scripts/verify-release.sh` and this page: commit those. The launcher is built with the key
   from `verify-release.sh` (`launcher/build.rs`) and checks its own updates with it, so commit
   the key **before the first release is tagged**: a launcher built without it never updates
   itself, and its players have to fetch the next release by hand.
2. On GitHub, in the repository's settings: add the `VT_API_KEY` secret (Secrets and variables ›
   Actions), turn on private vulnerability reporting (Security), and let Actions make releases
   and packages (Actions › General › Workflow permissions: read and write).
3. Sign the GitHub CLI in with the scopes the scripts need:
   `gh auth login` then `gh auth refresh -s write:packages`.

## CI

`.github/workflows/ci.yml` runs on every push to `main` and every pull request:
- on Linux: formatting, Clippy (warnings are errors) and the tests, as `scripts/check.sh` runs
  them; the test players against a fresh server (`scripts/bots.sh`: signing in, a friend request
  and a party invite, and a Quick Match hosted and joined); ShellCheck on the scripts; and the
  server's Docker image;
- on Windows: the client DLLs, their tests, the launcher's tests, and the launcher with the DLLs
  inside.

`.github/workflows/codeql.yml` runs CodeQL on the Rust code, the vendored C, the scripts and the
workflows, each week and on every change; results in test code are dropped first
(`scripts/codeql-drop-tests.py`).

### How the launcher updates itself

A release build of the launcher asks GitHub for the latest release when it starts and every 4
hours, and offers a newer one in a bar under its title. When the player says Update, it downloads
`SHA256SUMS` and `SHA256SUMS.sig` first and refuses the release unless the signature is the
release key's for **that tag's version**, then downloads `crosscom-launcher.exe` and refuses it
unless it matches its line in `SHA256SUMS`. Only a release newer than the running launcher is
installed, and never while the game runs. The new exe takes the old one's name (the old one is
kept as `crosscom-launcher.exe.old` until the next start) and starts; the client it carries is
installed when the player next presses Play. So a release is offered to players only once it's
signed: an unsigned draft, or files published again under a new tag, are refused. A development
build (`0.2.0-dev`) doesn't update itself.

A test build can point the launcher at another release and key: `CROSSCOM_UPDATE_FROM` (an
HTTPS address answering as GitHub's latest release) and `CROSSCOM_RELEASE_KEY` (the key's
public half, base64 DER, the middle line of its PEM) when it's built. `launcher/src/update.rs`
has two ignored tests (`cargo test -p crosscom-launcher -- --ignored live_`) for such a release.

## After a release

Update the community test network to the release's server image, by its digest from `IMAGE`, and
check `/api/info` reports the new version.
