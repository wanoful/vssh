#!/usr/bin/env python3
import json
import os
import sys
import urllib.error
import urllib.request
from vssh_bridge import connection, find_real_program


def fallback():
    real_code = find_real_program("code")
    if real_code:
        os.execv(real_code, [real_code] + sys.argv[1:])
    print(
        "vssh: LOCAL_CODE_BRIDGE is not set. Start this shell with `vssh <host>`.",
        file=sys.stderr,
    )
    return 127


def main():
    try:
        current = connection()
    except Exception as error:
        print("vssh: {}".format(error), file=sys.stderr)
        return 1
    bridge, token, host = current["bridge"], current["token"], current["host"]
    if not bridge or not token or not host:
        return fallback()

    payload = {
        "host": host,
        "cwd": os.getcwd(),
        "args": sys.argv[1:],
    }
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        bridge.rstrip("/") + "/open",
        data=body,
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )

    try:
        with urllib.request.urlopen(request, timeout=None) as response:
            response.read()
        return 0
    except urllib.error.HTTPError as error:
        message = error.read().decode("utf-8", errors="replace")
        print("vssh: local code bridge rejected the request:", message, file=sys.stderr)
        return 1
    except Exception as error:
        print("vssh: failed to reach local code bridge: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
