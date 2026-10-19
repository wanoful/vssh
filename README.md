# vssh

`vssh` is an SSH wrapper that lets remote shell commands open your local VS Code
through Remote-SSH (`code .`) or download remote files to your computer (`vdown`).

The remote `code` command is a small shim. It sends an authenticated request over
an SSH reverse tunnel to a local bridge. The local bridge runs the real VS Code
CLI:

```sh
code --remote ssh-remote+<host> <remote-path>
```

## Build

```sh
cargo build --release
```

## Install the remote shim

Install `~/.local/bin/code` and `~/.local/bin/vdown` on the remote host
(requires Python 3.7 or newer). Run this again after upgrading to add `vdown`:

```sh
vssh install-shim myhost
```

Make sure `~/.local/bin` is before any real `code` binary in the remote `PATH`.

## Optional Zellij integration

Install the Zellij wrapper explicitly (the default install does not install it):

```sh
vssh install-shim --with-zellij myhost
```

Then connect and attach normally, without a separate binding command:

```sh
vssh myhost
# On the remote host:
zellij attach -c work
vdown report.pdf
code .
```

The wrapper records the attaching vssh connection for that Zellij session.
`vdown` and `code` read the association on each invocation, so existing panes
use the new bridge after reconnecting and attaching again, even if they retain
old or missing vssh environment variables. Different Zellij sessions are independent.
When multiple vssh connections attach the same session, the last attach selects
the local computer for all its panes. Detaching does not restore an older target;
reattach from the desired vssh connection to select it again. An SSH connection
must remain open for its tunnel to work.

Supported forms include `zellij attach NAME`, `zellij a -c NAME`, `zellij -s NAME`,
and unnamed attach when there is one unambiguous session. Plain `zellij` and
`zellij attach -c` with no existing sessions create a name beginning with `vssh-`.
With multiple sessions, use a name instead of an interactive session picker.
Switching sessions via the Zellij UI, renaming sessions, and Zellij's HTTPS remote
client are not tracked; reattach by name through the wrapper when changing sessions.

The real Zellij must be installed on the remote host. Keep `~/.local/bin` first in
`PATH` and invoke `zellij` by name so the wrapper runs. If a real executable already
occupies `~/.local/bin/zellij`, the installer preserves it as `zellij.vssh-real`;
otherwise the wrapper finds Zellij elsewhere in `PATH`. Without a vssh connection,
the wrapper delegates unchanged. Rerun installation with `--with-zellij` when upgrading
the wrapper. Omitting the flag leaves an already installed wrapper in place.

Associations are stored under `~/.local/state/vssh/zellij` with private directory
and file permissions. To remove the integration, remove only `~/.local/bin/zellij`
and restore `zellij.vssh-real` to `zellij` if that backup exists.
Remove `~/.local/state/vssh/zellij` as well to clear associations used by existing panes.

## Connect

Start SSH through `vssh`:

```sh
vssh myhost
```

Suppress the bridge startup line with quiet mode:

```sh
vssh --quiet myhost
```

By default, each session chooses a random high remote loopback port so multiple
`vssh` sessions can connect to the same host at the same time. For debugging or
special cases, force a specific remote port:

```sh
vssh --remote-port 39045 myhost
```

Inside that remote shell:

```sh
code .
code file.rs
code -g src/main.rs:42:1
code -r .
```

If the SSH target name is different from the VS Code Remote-SSH host alias, pass
the VS Code alias explicitly:

```sh
vssh --code-host devbox-alias user@example.com
```

Raw SSH options can be passed after `--`:

```sh
vssh myhost -- -p 2222
```

When `-p PORT`, `-pPORT`, or the equivalent `-o Port=PORT` is used, `vssh`
also includes that port in the VS Code Remote-SSH target using Remote-SSH's
encoded authority format. This keeps a remote `code .` connection on the same
SSH port as the shell opened by `vssh` when connecting to a direct hostname.

VS Code Remote-SSH currently discards an authority port after it matches a
`Host` alias in the local SSH configuration. For an alias, set `Port` in the
same configuration block so both `vssh` and VS Code use it:

```sshconfig
Host myhost
    HostName server.example.com
    User me
    IdentityFile ~/.ssh/id_ed25519
    Port 2222
```

On Windows, `vssh` searches `PATH` and `PATHEXT` for the VS Code CLI, so the
usual `code.cmd` shim is supported. If VS Code is not on `PATH`, pass it
explicitly:

```powershell
.\vssh.exe --code-bin "$env:LOCALAPPDATA\Programs\Microsoft VS Code\bin\code.cmd" myhost
```

## Download remote files

After upgrading, rebuild the local binary and rerun `vssh install-shim myhost`
to update the remote command, then reconnect with the updated `vssh`.

Inside a remote shell started with `vssh`, run:

```sh
vdown report.pdf
vdown file1.zip file2.csv
vdown *.log
vdown -r logs
vdown -rf logs -d '~/Downloads/project'
```

Files go to your **local system Downloads directory**, including a custom directory
configured in your OS. If the OS provides no Downloads location, `~/Downloads` on
this local computer is used. The directory is created if needed.

Specify a local destination directory with `-d` / `--to`, or a final `-- DIR`:

```sh
vdown file1 file2 -d '~/Downloads/project'
vdown file1 file2 -- /local/path
vdown file1 --to 'C:\Users\me\Downloads'
```

Quote local `~` paths so the remote shell does not expand them to the remote home.
Relative local destinations are resolved against the directory where you started
`vssh`. All plain arguments are remote source files; the last plain argument is
never guessed to be a local destination. For source names starting with `-`, use
`./-filename` (`--` is reserved for the local destination).

`vdown` streams file contents over the existing authenticated SSH reverse tunnel;
it needs no additional SSH connection, scp/rsync, or local VS Code installation.
It reports the saved local path after each file finishes. Files retain their base
names. Use `-r` / `--recursive` to download directories, preserving their top-level
directory, nested structure, and empty directories (for example, `vdown -r logs`
creates `Downloads/logs/`). Existing directories are merged. Directory symlinks
are not followed and are reported as failures; symlinks to regular files copy the
target contents. Special files such as sockets and FIFOs are rejected.

Existing files are refused by default. Use `-f` / `--force` to replace them, or
combine both options as `-rf`. Replacement happens only after the entire file
arrives, so a failed transfer preserves the old file. `-f` does not remove existing
directories or resolve file/directory conflicts. Failed or interrupted transfers
remove their temporary files. With multiple files, failures
are reported individually, remaining files are attempted, and any failure gives a
nonzero exit status. Files already completed remain saved. File permissions and
timestamps are not copied.

## Security Model

`vssh` binds the bridge to `127.0.0.1`, creates a per-session random token, and
uses SSH reverse forwarding to expose the bridge only to the remote loopback
address. The bridge translates a small allowlist of VS Code CLI arguments and
invokes the configured local `code` binary. Download requests stream bytes into
local files without executing remote-provided commands. Filenames must be plain
base names, including each component of a recursive path. Recursive paths reject
parent traversal and existing directory symlinks within the downloaded tree.
Completed files replace existing files only when `-f` is explicitly supplied.
An authenticated remote shell can choose a local destination directory writable
by your local user, so use `vssh` with remote hosts you trust.

Supported remote `code` forms:

- `code`
- `code .`
- `code <path>`
- `code -r|--reuse-window <path>`
- `code -n|--new-window <path>`
- `code -g|--goto <file:line[:column]>`
- `code --diff <left> <right>`
- `code --add <folder>`
- `code --wait <path>`

Unknown flags are rejected by the local bridge.

## Tests

```sh
cargo test
python3 -m unittest discover -s tests
```

On a POSIX host with Zellij installed, verify reattaching an actual persistent session and routing
both commands from its old pane to the new connection:

```sh
cargo build --release
python3 tests/zellij_smoke.py target/release/vssh
```
