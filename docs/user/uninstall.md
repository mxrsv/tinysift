# Uninstalling apps

`tiny uninstall` removes an app from `/Applications` together with the data it left in your
`~/Library`. It shows what it found and asks before removing anything. It needs macOS.

```bash
tiny uninstall                       # pick apps from a list
tiny uninstall AltTab                # one app by name
tiny uninstall AltTab --dry-run      # report only
tiny uninstall AltTab --shallow      # only the .app
tiny uninstall AltTab --leftovers-only   # only the ~/Library data, keep the app
tiny uninstall AltTab -y             # no menu, Move to Trash now
```

## Picking apps

Without a name you get every app in `/Applications`, with its size and when you last opened
it. `--sort` orders the list: `last-used` (default, least recently used first), `size` or
`name`.

## What is removed

The `.app` bundle, plus whatever exists of these for the app's bundle identifier:
Application Support, Caches, Preferences, Containers, Saved Application State, HTTP storage
and cookies, WebKit data, LaunchAgents and Group Containers — all under `~/Library`.

If `tiny` cannot read the app's bundle identifier, or it is not a valid reverse-DNS name
(at least two dot-separated parts of letters, digits and `-`, with no `/` or `..`), it removes
only the `.app`, because guessing by name could hit folders that belong to macOS. Leftovers are
looked up under your account's home folder from the user database, not the `HOME` variable, and
a leftover that is a symbolic link is skipped rather than followed.

## Choosing an action

After the report:

- **Move to Trash (recoverable)** — the default.
- **Dry-run (no changes)**.
- **Hard delete (NOT recoverable)** — asks you to confirm once more. `--hard` preselects it.
- **Cancel**.

`-y` skips the menu and moves to the Trash. **`-y --hard` deletes permanently with no
confirmation at all** — unlike `tiny clean`, no environment variable is required.

## What is refused

- Apple's own apps, recognised by a `com.apple.` bundle identifier, or by name when the
  identifier cannot be read.
- Homebrew casks, recognised only when `/opt/homebrew/Caskroom/<app name>` exists, with the
  name in lower case and spaces as hyphens. Use `brew uninstall --cask <name>`, or pass
  `--force` to remove them with `tiny` anyway. A cask installed under another prefix or
  another name is not recognised and is removed like any other app.

If any selected app is refused, `tiny` stops after the report and removes nothing.

If one app fails partway, the others still run and `tiny` lists the failure at the end; the
failed app may be partly removed.
