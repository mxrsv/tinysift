# Security policy

TinySift removes files, quits processes and uninstalls apps. The rules that decide what may be
touched live in this repository, so a way around one of them is a security problem, not an
ordinary bug.

## Reporting a vulnerability

Report it privately. Do not open a public issue or discussion for a problem that is not fixed yet.

1. Preferred: [report a vulnerability](https://github.com/mxrsv/tinysift/security/advisories/new)
   through GitHub's private reporting. Only the maintainer sees it.
2. Or email `hello@tinysift.app` with the subject `TinySift security report`.

Include what you can of:

- the TinySift app version or the CLI's `--version` output, and how you installed it;
- the macOS version and whether the Mac is Apple silicon;
- the exact command, screen or steps involved;
- a way to reproduce it that does not need your real files, such as a temporary folder;
- what was removed, read or run that should not have been.

## What counts

- Removal outside what the review showed and you confirmed: a path that was not listed, a
  symlink that was followed, a move to another volume, or a rule in your protection list that
  was ignored ([clean contract](docs/internals/clean.md)).
- Anything that runs a command, or gains administrator rights, from a path or name found on disk.
- A preview or consent that can be reused, forged or made to outlive its expiry.
- The integrity of a release: a download that is not the signed and notarized build named in the
  [release guide](docs/operations/release.md), or an update that installs something else.

A category that finds too little, a wrong size or a crash that removes nothing is a normal bug:
[open an issue](https://github.com/mxrsv/tinysift/issues/new/choose).

## What to expect

TinySift has one maintainer, so these are best-effort, and security reports come before other work.

- An acknowledgement within 7 days.
- A status update within 30 days when a fix or mitigation is not ready by then.
- A fix shipped as a new version, with credit to you in its release notes unless you prefer none.
  Disclosure is coordinated with you once the fix, or clear guidance for users, is out.

## Supported versions

Fixes go into the latest release and the `main` branch. Earlier releases are not patched; the fix
is a new version.
