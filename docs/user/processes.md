# Processes

`tiny processes` lists what is running, shows one process in detail, finds who listens on a
TCP port, and quits a process you own. The [desktop app](../../../tiny-cli/docs/user/desktop.md) shows the same data
grouped by app.

```bash
tiny processes                          # top 20 by CPU
tiny processes --sort memory --limit 50
tiny processes --port 3000              # visible owners of a listening TCP port
tiny processes show 4321                # parent, children and listening ports
tiny processes quit 4321                # ask it to quit (SIGTERM), after a prompt
tiny processes quit 4321 --force        # end it at once (SIGKILL), after a prompt
TINY_CONFIRM_FORCE=1 tiny processes quit 4321 --force -y   # no prompt
```

## Reading the list

CPU is per core, so a busy process can exceed 100%. `n/a` means `tiny` could not measure the
value or is not allowed to read it. `--limit` (default 20) caps the list; `--port` ignores
it and lists every visible owner.

`--json` on the list, on `--port` and on `show` prints camelCase fields instead of a table.
A CPU value that was not measured is `null`, never `0`.

## Ports

`--port` lists only owners the current user can see. Without root, other users' sockets are
invisible, so no owner does not mean the port is free.

## Quitting

`quit` asks first; the default answer is No. It sends exactly one signal, never escalates
from SIGTERM to SIGKILL and never signals a process's children. Just before signalling it
checks that the PID still belongs to the same process.

`tiny` refuses to quit itself, its parent, PID 0 or 1, other users' processes and
`WindowServer`, `loginwindow`, `Dock`, `SystemUIServer` and `Finder`.

`quit` exits with an error unless the process actually exited within a couple of seconds. If
it is still running after SIGTERM, try `--force`. `--force -y` skips the prompt, so it is
refused unless `TINY_CONFIRM_FORCE=1` is set.
