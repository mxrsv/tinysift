# Scanning files

`tiny scan` reports large and old files so you can decide what to keep. It never deletes,
moves or changes anything. For removing caches, see [Cleaning caches](clean.md).

```bash
tiny scan                                   # ~/Downloads, ~/Desktop, ~/Documents
tiny scan --min-size-mb 500 --older-than-days 365
tiny scan --path ~/Movies --path ./assets   # scan these folders instead
tiny scan --by-ext --duplicates             # add breakdowns
tiny scan --json > report.json              # machine-readable
```

## What is reported

- **Large files** — at or above `--min-size-mb` (default 100).
- **Old files** — not modified for `--older-than-days` (default 90).
- **By extension** (`--by-ext`) — file count and total size per extension.
- **Duplicates** (`--duplicates`) — files sharing both size and filename, with the space the
  extra copies take. Add `--hash` to confirm groups by comparing content; slower, and drops
  files that only looked alike.

Each list shows `--limit` entries (default 20), ordered by `--sort`: `size` (largest first,
default), `age` (oldest first) or `path`.

## Which folders

By default the three folders above, skipping any that do not exist. Each `--path` replaces
that set; relative paths are resolved from the current directory. Explicit paths work without
`HOME`. Repeated or overlapping roots count each file path once; an explicitly named nested
folder still gets scanned even when its name is normally skipped beneath another root.

Missing, unreadable or non-directory roots fail the command. Paths containing `..` or passing
through a symbolic link (such as `/tmp` on macOS) are refused before normalization. An
unreadable directory, entry or file timestamp also fails instead of producing a successful
partial report; errors go to stderr and JSON stdout stays empty. See the
[root validation](../../crates/tiny-core/src/scan/mod.rs) and
[walker](../../crates/tiny-core/src/scan/walker.rs).

Below each scan root, `tiny` skips version-control folders, build and package output (`node_modules`,
`target`, `dist`, `.venv` and similar), macOS metadata folders, and anything deeper than 12
levels.

## `.tinyignore`

Put a `.tinyignore` in a scanned folder or in your home folder to skip more:

```
# any folder named Archive
Archive
# any file with this extension (case-sensitive)
*.iso
```

Only these two forms are understood, one per line; a `#` starts a comment only at the
beginning of a line. It is not a full `.gitignore`. Rules from every file found apply to the
whole scan.

## JSON output

`--json` prints one JSON object on stdout with `roots`, `thresholds`, `large_files`,
`old_files` and `totals`, plus `by_extension` and `duplicates` when those flags are set.
`--limit` applies to the lists here too.
