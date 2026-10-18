#!/usr/bin/env python3
"""Stream remote files to the local vssh bridge using its SSH reverse tunnel."""
import argparse
import json
import os
import stat
import sys
import urllib.error
import urllib.request


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Download remote files to your local Downloads directory.",
        epilog="Use -d DIR or a final -- DIR for a local destination. Quote local '~' paths.",
    )
    parser.add_argument("-d", "--to", metavar="DIR", help="local destination directory")
    parser.add_argument("files", nargs="+", help="remote files (shell globs supported)")
    # A final `-- DIR` is a destination; use ./-name for filenames starting with '-'.
    destination = None
    if "--" in argv:
        index = argv.index("--")
        if len(argv[index + 1:]) != 1:
            parser.error("expected exactly one local destination directory after --")
        destination = argv[index + 1]
        argv = argv[:index]
    args = parser.parse_intermixed_args(argv)
    if destination is not None:
        if args.to is not None:
            parser.error("specify the local destination only once")
        args.to = destination
    return args


def main():
    args = parse_args(sys.argv[1:])
    bridge = os.environ.get("LOCAL_CODE_BRIDGE")
    token = os.environ.get("LOCAL_CODE_TOKEN")
    if not bridge or not token:
        print("vdown: start this shell with `vssh <host>`.", file=sys.stderr)
        return 1

    # Do not send session credentials through an HTTP proxy or follow redirects.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    failed = False
    for source in args.files:
        try:
            if not stat.S_ISREG(os.stat(source).st_mode):
                raise ValueError("only regular files are supported")
            with open(source, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode):
                    raise ValueError("only regular files are supported")
                request = urllib.request.Request(
                    bridge.rstrip("/") + "/download",
                    data=stream,
                    method="POST",
                    headers={
                        "Authorization": "Bearer " + token,
                        "Content-Type": "application/octet-stream",
                        "Content-Length": str(info.st_size),
                        "X-Vssh-Download": json.dumps({
                            "name": os.path.basename(os.path.abspath(source)),
                            "destination": args.to,
                        }),
                    },
                )
                with opener.open(request, timeout=None) as response:
                    result = json.load(response)
                print("vdown: {} -> {}".format(source, result["path"]), flush=True)
        except urllib.error.HTTPError as error:
            message = error.read().decode("utf-8", errors="replace")
            print("vdown: {}: {}".format(source, message), file=sys.stderr)
            failed = True
        except Exception as error:
            print("vdown: {}: {}".format(source, error), file=sys.stderr)
            failed = True
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
