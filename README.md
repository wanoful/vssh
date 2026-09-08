# vssh

`vssh` is an SSH wrapper that lets a remote shell command like `code .` open your
local VS Code through Remote-SSH.

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

Install `~/.local/bin/code` on the remote host:

```sh
vssh install-shim myhost
```

Make sure `~/.local/bin` is before any real `code` binary in the remote `PATH`.

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
also includes that port in the VS Code Remote-SSH target. This keeps a remote
`code .` connection on the same SSH port as the shell opened by `vssh`.

On Windows, `vssh` searches `PATH` and `PATHEXT` for the VS Code CLI, so the
usual `code.cmd` shim is supported. If VS Code is not on `PATH`, pass it
explicitly:

```powershell
.\vssh.exe --code-bin "$env:LOCALAPPDATA\Programs\Microsoft VS Code\bin\code.cmd" myhost
```

## Security Model

`vssh` binds the bridge to `127.0.0.1`, creates a per-session random token, and
uses SSH reverse forwarding to expose the bridge only to the remote loopback
address. The bridge does not execute arbitrary remote-provided commands. It only
translates a small allowlist of VS Code CLI arguments and invokes the configured
local `code` binary.

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
