"""Real Zellij reattach smoke test: python3 tests/zellij_smoke.py target/release/vssh."""
import http.server
import json
import os
from pathlib import Path
import pty
import shlex
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
from vssh_bridge import find_real_program


def wait_for(check, description):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise AssertionError("timed out: " + description)


def start_client(shim, args, environment):
    fd, slave = pty.openpty()
    import fcntl
    import termios
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
    # Set the controlling terminal in a fresh interpreter, avoiding fork-with-threads.
    setup = "import fcntl,termios,os,sys; fcntl.ioctl(0,termios.TIOCSCTTY,0); os.execv(sys.argv[1],sys.argv[1:])"
    client = subprocess.Popen([sys.executable, "-c", setup, str(shim)] + args,
                              env=environment, stdin=slave, stdout=slave, stderr=slave,
                              start_new_session=True)
    os.close(slave)
    output = bytearray()
    def drain():
        try:
            while True:
                data = os.read(fd, 65536)
                if not data:
                    break
                output.extend(data)
                del output[:-4000]
        except OSError:
            pass
    threading.Thread(target=drain, daemon=True).start()
    return client, fd, output


def stop_client(client, fd):
    try:
        os.killpg(client.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    client.wait(timeout=5)
    os.close(fd)


def main():
    real = find_real_program("zellij")
    if not real:
        raise SystemExit("zellij is required")
    binary = str(Path(sys.argv[1]).resolve())
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            data = self.rfile.read(int(self.headers["Content-Length"]))
            requests.append((self.server.label, self.path, self.headers["Authorization"], data))
            payload = json.dumps({"ok": True, "path": "local-" + self.server.label}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    servers = [http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler) for _ in range(2)]
    for label, server in zip(["A", "B"], servers):
        server.label = label
        threading.Thread(target=server.serve_forever, daemon=True).start()
    clients = []
    with tempfile.TemporaryDirectory(prefix="vssh-zellij-smoke-") as temporary:
        root = Path(temporary)
        env = dict(os.environ, HOME=temporary, ZELLIJ_SOCKET_DIR=str(root / "sockets"),
                   ZELLIJ_CONFIG_DIR=str(root / "config"), TERM="xterm-256color")
        for key in ["ZELLIJ", "ZELLIJ_SESSION_NAME", "LOCAL_CODE_BRIDGE", "LOCAL_CODE_TOKEN", "LOCAL_CODE_SSH_HOST"]:
            env.pop(key, None)
        (root / "config").mkdir()
        (root / "config/config.kdl").write_text("session_serialization false\nweb_server false\nshow_release_notes false\nshow_startup_tips false\n")
        fake_ssh = root / "ssh"
        fake_ssh.write_text("#!/bin/sh\nexec sh -s\n")
        fake_ssh.chmod(0o700)
        subprocess.run([binary, "install-shim", "--with-zellij", "--ssh-bin", str(fake_ssh), "test"], env=env, check=True)
        shim = root / ".local/bin/zellij"
        env["PATH"] = str(shim.parent) + os.pathsep + env["PATH"]
        def connection(label, server):
            return dict(env, LOCAL_CODE_BRIDGE="http://127.0.0.1:{}".format(server.server_port),
                        LOCAL_CODE_TOKEN="fake-" + label, LOCAL_CODE_SSH_HOST="host-" + label)
        a, b = connection("A", servers[0]), connection("B", servers[1])
        session = None
        session_file = root / "session-name"
        source = root / "remote.txt"
        source.write_bytes(b"remote-file-content")
        ready = root / "ready"
        done = root / "done"
        startup = 'printf %s "$ZELLIJ_SESSION_NAME" > {}; {} {}; printf ready > {}; exec /bin/sh'.format(
            shlex.quote(str(session_file)), shlex.quote(str(shim.parent / "vdown")),
            shlex.quote(str(source)), shlex.quote(str(ready)))
        shell = root / "test-shell"
        shell.write_text("#!/bin/sh\n" + startup + "\n")
        shell.chmod(0o700)
        with (root / "config/config.kdl").open("a") as config:
            config.write("default_shell " + json.dumps(str(shell)) + "\n")
        try:
            initial_client, initial_fd, initial_output = start_client(shim, ["--layout-string", "layout { pane; }"], a)
            clients.append((initial_client, initial_fd))
            try:
                wait_for(ready.exists, "initial pane and first download")
            except AssertionError:
                print("initial child status:", initial_client.poll())
                print("initial client output (tail):", repr(bytes(initial_output)))
                print("session name:", session_file.read_text() if session_file.exists() else "not created", "requests:", requests)
                raise
            session = session_file.read_text()
            assert session and not session.startswith("vssh-"), session
            assert len(requests) == 1 and requests[0][:3] == ("A", "/download", "Bearer fake-A"), requests
            requests.clear()
            bindings = list((root / ".local/state/vssh/zellij").glob("*.json"))
            assert len(bindings) == 1
            assert json.loads(bindings[0].read_text())["token"] == "fake-A"
            # Disconnect A, retaining its pane, then attach from B.
            stop_client(initial_client, initial_fd)
            clients.clear()
            client, master, terminal_output = start_client(shim, ["attach", session], b)
            clients.append((client, master))
            wait_for(lambda: json.loads(bindings[0].read_text())["token"] == "fake-B", "reattach binding")
            try:
                wait_for(lambda: len(subprocess.run([real, "-s", session, "action", "list-clients"], env=b, capture_output=True, text=True, timeout=5).stdout.splitlines()) > 1, "attached client")
            except AssertionError:
                print("client output (tail):", repr(bytes(terminal_output[-2000:])))
                print("clients:", subprocess.run([real, "-s", session, "action", "list-clients"], env=b, capture_output=True, text=True, timeout=5))
                print("panes:", subprocess.run([real, "-s", session, "action", "list-panes", "--all"], env=b, capture_output=True, text=True, timeout=5))
                raise
            pane_id = "terminal_0"
            command = "{} {}; {} {}; printf done > {}\r".format(
                shlex.quote(str(shim.parent / "vdown")), shlex.quote(str(source)),
                shlex.quote(str(shim.parent / "code")), shlex.quote(str(source)), shlex.quote(str(done)))
            subprocess.run([real, "-s", session, "action", "write-chars", "--pane-id", pane_id, command], env=b, check=True, timeout=10)
            try:
                wait_for(done.exists, "downloads and code from old pane")
            except AssertionError:
                diagnostic = subprocess.run([real, "-s", session, "action", "dump-screen", "--pane-id", pane_id], env=b, capture_output=True, text=True, timeout=5)
                print("pane output:", diagnostic.stdout, diagnostic.stderr, "requests:", requests)
                raise
            assert len(requests) == 2, requests
            assert all(label == "B" and token == "Bearer fake-B" for label, path, token, data in requests), requests
            assert requests[0][1:] == ("/download", "Bearer fake-B", b"remote-file-content"), requests
            payload = json.loads(requests[1][3])
            assert payload["host"] == "host-B", payload
            print("PASS: native random name preserved; old pane routes vdown and code to reattaching connection B")
        finally:
            if session is None and session_file.exists():
                session = session_file.read_text()
            if session:
                subprocess.run([real, "kill-session", session], env=env, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=10)
            for pid, fd in clients:
                stop_client(pid, fd)
            for server in servers:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    main()
