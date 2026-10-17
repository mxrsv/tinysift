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

## What is listed

The `.app` bundle, plus these exact entries for the app's bundle identifier, all under
`~/Library`: `Application Support/<id>`, `Caches/<id>`, `Containers/<id>`, `HTTPStorages/<id>`,
`WebKit/<id>`, `Preferences/<id>.plist`, `Saved Application State/<id>.savedState`,
`HTTPStorages/<id>.binarycookies` and `LaunchAgents/<id>.plist`. `Logs` is not touched.

The report labels every line:

- `app` — the `.app` bundle.
- `cache` — `Caches/<id>`; the app rebuilds it.
- `data` — every other exact entry above (settings, app data, cookies, saved state, LaunchAgents).
- `report-only` — shown with a reason but **never removed**: LaunchAgents named
  `<id>.<suffix>.plist`, Group Containers named `<id>`, `group.<id>`, `<anything>.<id>`,
  `<id>.<anything>` or containing `.<id>.` (they are shared between apps), and every exact
  entry when another installed app has the same bundle identifier or `/Applications` could not
  be listed. Names that merely start with or contain the identifier without a `.` boundary
  (`<id>Helper`) are not listed.
- `uncertain` — shown with a reason but **never removed**: the entry, or its parent folder, is
  a symbolic link rather than a real file or folder.

Folders derived from the app's display name, top-level dot folders and system `/Library` are
never looked at. The subtotal counts only what an action can remove.

If `tiny` cannot read the app's bundle identifier, or it is not a valid reverse-DNS name
(at least two dot-separated parts of letters, digits and `-`, with no `/` or `..`), it lists
only the `.app`, because guessing by name could hit folders that belong to macOS. Leftovers are
looked up under your account's home folder from the user database, not the `HOME` variable.

## Choosing an action

After the report:

- **Move to Trash (recoverable)** — the default.
- **Dry-run (no changes)**.
- **Hard delete (NOT recoverable)** — asks you to confirm once more. `--hard` preselects it.
- **Cancel**.

Choosing Move to Trash or Hard delete in the menu removes the app and all `cache` and `data`
entries. `report-only` and `uncertain` entries are never removed by any action.

`-y` skips the menu and moves to the Trash, but only the app and `cache` entries: `data`
entries are kept and the output says how many. Run without `-y` to remove them. **`-y --hard`
deletes the app and the `cache` entries permanently with no confirmation at all** — unlike
`tiny clean`, no environment variable is required.

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
