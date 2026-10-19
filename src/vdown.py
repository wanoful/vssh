#!/usr/bin/env python3
"""Stream remote files to the local vssh bridge using its SSH reverse tunnel."""
import argparse
import contextlib
import json
import os
import stat
import sys
import urllib.error
import urllib.request
from vssh_bridge import connection


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Download remote files to your local Downloads directory.",
        epilog="Use -d DIR or a final -- DIR for a local destination. Quote local '~' paths.",
    )
    parser.add_argument("-d", "--to", metavar="DIR", help="local destination directory")
    parser.add_argument("-r", "--recursive", action="store_true", help="download directories recursively")
    parser.add_argument("-f", "--force", action="store_true", help="overwrite existing local files")
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
    try:
        current = connection()
    except Exception as error:
        print("vdown: {}".format(error), file=sys.stderr)
        return 1
    bridge, token = current["bridge"], current["token"]
    if not bridge or not token:
        print("vdown: start this shell with `vssh <host>`.", file=sys.stderr)
        return 1

    # Do not send session credentials through an HTTP proxy or follow redirects.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    def download(source, relative_dir):
        try:
            info = os.stat(source)
            is_directory = stat.S_ISDIR(info.st_mode)
            if is_directory:
                if not args.recursive:
                    raise ValueError("directory requires -r / --recursive")
                if os.path.islink(source):
                    raise ValueError("directory symlinks are not followed")
            elif not stat.S_ISREG(info.st_mode):
                raise ValueError("only regular files and directories are supported")
            name = os.path.basename(os.path.abspath(source))
            # Empty bodies create directories, preserving empty directories as well.
            resource = contextlib.nullcontext(None) if is_directory else open(source, "rb")
            with resource as stream:
                if stream is not None:
                    info = os.fstat(stream.fileno())
                    if not stat.S_ISREG(info.st_mode):
                        raise ValueError("only regular files are supported")
                request = urllib.request.Request(
                    bridge.rstrip("/") + "/download",
                    data=b"" if is_directory else stream,
                    method="POST",
                    headers={
                        "Authorization": "Bearer " + token,
                        "Content-Type": "application/octet-stream",
                        "Content-Length": "0" if is_directory else str(info.st_size),
                        "X-Vssh-Download": json.dumps({
                            "name": name,
                            "destination": args.to,
                            "relative_dir": relative_dir,
                            "directory": is_directory,
                            "force": args.force,
                        }),
                    },
                )
                with opener.open(request, timeout=None) as response:
                    result = json.load(response)
                print("vdown: {} -> {}".format(source, result["path"]), flush=True)
            failed = False
            if is_directory:
                with os.scandir(source) as entries:
                    for entry in entries:
                        # Attempt every sibling even if another entry fails.
                        if not download(entry.path, relative_dir + [name]):
                            failed = True
            return not failed
        except urllib.error.HTTPError as error:
            message = error.read().decode("utf-8", errors="replace")
            print("vdown: {}: {}".format(source, message), file=sys.stderr)
        except Exception as error:
            print("vdown: {}: {}".format(source, error), file=sys.stderr)
        return False

    failed = False
    for source in args.files:
        if not download(source, []):
            failed = True
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
