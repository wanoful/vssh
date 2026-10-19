"""Shared remote bridge discovery; embedded into each installed shim."""
import hashlib
import json
import os
import stat
import sys
import tempfile


def find_real_program(name):
    own_path = os.path.realpath(sys.argv[0])
    candidates = []
    if name == "zellij":
        candidates.append(os.path.join(os.path.dirname(own_path), "zellij.vssh-real"))
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        if directory:
            candidates.append(os.path.join(directory, name))
    for candidate in candidates:
        if os.path.realpath(candidate) != own_path and os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def _binding_path(session):
    # Separate Zellij servers using different socket directories on the same host.
    scope = os.environ.get("ZELLIJ_SOCKET_DIR") or os.environ.get("XDG_RUNTIME_DIR") or "default"
    key = hashlib.sha256(json.dumps([scope, session]).encode("utf-8")).hexdigest()
    directory = os.path.expanduser("~/.local/state/vssh/zellij")
    return directory, os.path.join(directory, key + ".json")


def _check_private(path, directory=False):
    info = os.lstat(path)
    expected_type = stat.S_ISDIR if directory else stat.S_ISREG
    if not expected_type(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise RuntimeError("unsafe vssh binding permissions: {}".format(path))


def bind_session(session, current):
    if not all(current.get(key) for key in ("bridge", "token", "host")):
        return
    directory, path = _binding_path(session)
    os.makedirs(directory, mode=0o700, exist_ok=True)
    _check_private(directory, directory=True)
    fd, temporary = tempfile.mkstemp(prefix=".binding-", dir=directory)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(current, stream)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def connection():
    session = os.environ.get("ZELLIJ_SESSION_NAME") if "ZELLIJ" in os.environ else None
    if session:
        directory, path = _binding_path(session)
        try:
            _check_private(directory, directory=True)
            _check_private(path)
            with open(path, encoding="utf-8") as stream:
                current = json.load(stream)
            if not isinstance(current, dict) or not all(
                isinstance(current.get(key), str) and current[key]
                for key in ("bridge", "token", "host")
            ):
                raise ValueError("incomplete connection data")
            return current
        except FileNotFoundError:
            pass
        except Exception as error:
            raise RuntimeError("cannot read Zellij vssh binding; reattach through vssh: {}".format(error)) from error
    return {
        "bridge": os.environ.get("LOCAL_CODE_BRIDGE"),
        "token": os.environ.get("LOCAL_CODE_TOKEN"),
        "host": os.environ.get("LOCAL_CODE_SSH_HOST"),
    }
