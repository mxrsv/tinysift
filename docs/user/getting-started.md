# Getting started

`sift` is a small command-line tool for everyday upkeep of a Mac: what the machine is doing,
which files are taking space, which caches can go, which apps left data behind and which
processes are running. Every command that can remove something shows you a plan first and
defaults to Move to Trash. Processes and cache cleanup also have a [desktop app](https://tinysift.app/docs).

## Install

`sift` is built from source. You need Rust 1.91 or newer (`rustup` installs it). From the
root of a checkout of this repository:

```bash
cargo install --path crates/tiny     # puts `tinysift` on your PATH (~/.cargo/bin)
ln -s tinysift ~/.cargo/bin/sift     # the short name every guide uses
```

If another `sift` is already on your Mac, skip the second line and type `tinysift` wherever a guide
says `sift`: it is the same tool.

To try it without installing, replace `sift` with `cargo run -p tinysift --` in every example.

`sys`, `scan` and `focus` only read your system or write their own log. `clean` and
`uninstall` move things to the Trash through Finder, so they need macOS; the first run may
ask you to allow your terminal to control Finder.

## Commands

The [command table](../../README.md#commands) links each command to its guide.
`sift --help` and `sift <command> --help` list every flag.

## `sys` — system information

```bash
sift sys
```

Prints one snapshot and exits. Nothing is written anywhere.

## `focus` — focus timer

```bash
sift focus                          # 25 minutes
sift focus --minutes 50 --label "deep work"
```

Shows a progress bar until the session ends, then appends it to
`~/.tiny-cli/focus-sessions.json`. Stopping early with Ctrl-C records nothing.
Completed sessions use a [locked, atomic update](../../crates/tiny-core/src/focus.rs),
so concurrent timers keep both entries and a failed write preserves the previous history.

If that file is not valid JSON, `sift` refuses to overwrite it: it copies the file to
`focus-sessions.json.bak` and asks you to fix or delete the original.
