## What and why

<!-- What changes, for whom, and the problem it solves. Link the issue if there is one. -->

## How it was tested

<!-- What you ran, and what you tried by hand. -->

- [ ] `scripts/check.sh` passes: formatting, lints (warnings are errors) and tests
- [ ] Server changes: `scripts/bots.sh` passes (with a new `tools/testbot` scenario for new behaviour)
- [ ] Client or launcher changes: tried in the game (say on Windows or Wine, and with how many players), or said below why not

## Checklist

- [ ] No game files, nothing extracted from them, and no decompiled code
- [ ] Players or server hosts will notice this: a line in the next release's notes (`docs/releases/`)
- [ ] New data about players leaves their PC or is kept: said plainly where they see it, and in the README's "What's sent, and when"
- [ ] `release.toml`'s version left as it is
