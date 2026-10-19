#!/usr/bin/env python3
# vssh-zellij-shim
"""Automatically associate named Zellij sessions with the attaching vssh."""
import os
import subprocess
import sys
import uuid
from vssh_bridge import bind_session, connection, find_real_program


# Options which consume the next argument, before the attach subcommand.
GLOBAL_VALUES = {
    "--max-panes", "--data-dir", "-s", "--session", "-l", "--layout",
    "--layout-string", "-n", "--new-session-with-layout", "-c", "--config", "--config-dir",
}
ATTACH_VALUES = {"--index", "-t", "--token", "--ca-cert"}


def session_list(real, global_args):
    result = subprocess.run(
        [real] + global_args + ["list-sessions", "--short", "--no-formatting"],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    if result.returncode:
        return []
    return [line for line in result.stdout.splitlines() if line]


def global_options(args):
    """Read only enough global options to distinguish launches from CLI actions."""
    global_args = []
    named = None
    i = 0
    while i < len(args):
        arg = args[i]
        option, equal, value = arg.partition("=")
        if not equal and len(arg) > 2 and arg[:2] in GLOBAL_VALUES:
            option, equal, value = arg[:2], True, arg[2:]
        if option in GLOBAL_VALUES:
            if not equal:
                i += 1
                if i >= len(args):
                    return None
                value = args[i]
            if option in ("-n", "--new-session-with-layout"):
                return None
            if option in ("-s", "--session"):
                named = value
            if option in ("-c", "--config", "--config-dir", "--data-dir"):
                global_args.extend([option, value])
        elif arg in ("-h", "--help", "-V", "--version"):
            return None
        elif not arg.startswith("-"):
            break
        i += 1
    return named, global_args, i


def attach_target(real, args):
    """Return (session, forwarded args); leave non-interactive commands untouched."""
    args = list(args)
    options = global_options(args)
    if options is None:
        return None, args
    named, global_args, i = options
    if i == len(args):
        # A plain launch creates a new named session. An explicit -s is preserved.
        if not named:
            named = "vssh-" + uuid.uuid4().hex[:12]
            args = ["--session", named] + args
        return named, args
    if args[i] not in ("attach", "a"):
        return None, args
    i += 1
    target_index = None
    create = False
    index = None
    while i < len(args):
        arg = args[i]
        if arg == "--" or arg == "options":
            break
        if arg in ("-h", "--help"):
            return None, args
        option, equal, value = arg.partition("=")
        if option in ATTACH_VALUES:
            if not equal:
                i += 1
                if i >= len(args):
                    return None, args
                value = args[i]
            if option == "--index":
                index = value
        elif arg in ("-c", "--create", "-b", "--create-background"):
            create = True
        elif not arg.startswith("-"):
            if target_index is not None:
                return None, args
            target_index = i
        i += 1
    sessions = session_list(real, global_args)
    if target_index is not None:
        target = args[target_index]
        if "://" in target:
            return None, args  # Remote Zellij web clients use another host's files.
        # Resolve a unique prefix to the real session name used in existing panes.
        matches = [name for name in sessions if name.startswith(target)]
        if target not in sessions and len(matches) == 1:
            target = matches[0]
        return target, args
    if index is not None:
        try:
            position = int(index)
            if position < 0:
                return None, args
            return sessions[position], args
        except (ValueError, IndexError):
            return None, args
    if len(sessions) == 1:
        return sessions[0], args
    if not sessions and create:
        target = "vssh-" + uuid.uuid4().hex[:12]
        # Insert before '--' / options so initial commands keep their meaning.
        args.insert(i, target)
        return target, args
    print("vssh: automatic Zellij association needs an unambiguous session; use `zellij attach <name>`.", file=sys.stderr)
    return None, args


def main():
    real = find_real_program("zellij")
    if not real:
        print("vssh: real zellij executable not found in PATH or zellij.vssh-real.", file=sys.stderr)
        return 127
    args = sys.argv[1:]
    options = global_options(args)
    if options is None or (options[2] < len(args) and args[options[2]] not in ("attach", "a")):
        os.execv(real, [real] + args)
        return 0
    try:
        current = connection()
        if all(current.get(key) for key in ("bridge", "token", "host")):
            target, args = attach_target(real, args)
            if target:
                bind_session(target, current)
    except Exception as error:
        print("vssh: cannot associate Zellij session: {}".format(error), file=sys.stderr)
        return 1
    os.execv(real, [real] + args)


if __name__ == "__main__":
    raise SystemExit(main())
