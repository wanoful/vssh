"""Regression tests for remote Zellij discovery; run with unittest discovery."""
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))
import vssh_bridge as bridge
spec = importlib.util.spec_from_file_location("zellij_shim", Path(__file__).resolve().parents[1] / "src/zellij.py")
zellij = importlib.util.module_from_spec(spec)
spec.loader.exec_module(zellij)

A = {"bridge": "http://127.0.0.1:40001", "token": "fake-a", "host": "host-a"}
B = {"bridge": "http://127.0.0.1:40002", "token": "fake-b", "host": "host-b"}


@unittest.skipUnless(os.name == "posix", "remote shims target POSIX hosts")
class BindingTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.environment = patch.dict(os.environ, {
            "HOME": self.directory.name, "ZELLIJ": "0", "ZELLIJ_SESSION_NAME": "work",
            "LOCAL_CODE_BRIDGE": A["bridge"], "LOCAL_CODE_TOKEN": A["token"],
            "LOCAL_CODE_SSH_HOST": A["host"],
        }, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_existing_pane_reads_latest_attach_and_sessions_stay_independent(self):
        bridge.bind_session("work", A)
        bridge.bind_session("other", A)
        self.assertEqual(bridge.connection(), A)
        bridge.bind_session("work", B)
        self.assertEqual(bridge.connection(), B)  # Pane still has the original environment.
        os.environ["ZELLIJ_SESSION_NAME"] = "other"
        self.assertEqual(bridge.connection(), A)
        del os.environ["ZELLIJ"]
        self.assertEqual(bridge.connection(), A)  # Direct SSH shell keeps its own connection.

    def test_pane_created_before_vssh_needs_no_environment_refresh(self):
        for key in ("LOCAL_CODE_BRIDGE", "LOCAL_CODE_TOKEN", "LOCAL_CODE_SSH_HOST"):
            del os.environ[key]
        bridge.bind_session("work", B)
        self.assertEqual(bridge.connection(), B)

    def test_private_atomic_files_and_invalid_data_do_not_fall_back(self):
        bridge.bind_session("work", B)
        directory, filename = bridge._binding_path("work")
        self.assertEqual(stat.S_IMODE(os.stat(directory).st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(os.stat(filename).st_mode), 0o600)
        Path(filename).write_text("not json")
        with self.assertRaisesRegex(RuntimeError, "reattach"):
            bridge.connection()
        bridge.bind_session("work", B)
        os.chmod(filename, 0o644)
        with self.assertRaisesRegex(RuntimeError, "permissions"):
            bridge.connection()

    def test_socket_directories_and_path_like_session_names_are_isolated(self):
        bridge.bind_session("../work", B)
        directory, filename = bridge._binding_path("../work")
        self.assertEqual(Path(filename).parent, Path(directory))
        bridge.bind_session("work", B)
        os.environ["ZELLIJ_SOCKET_DIR"] = "/another/server"
        self.assertEqual(bridge.connection(), A)


class InvocationTests(unittest.TestCase):
    def target(self, args, sessions=("work",)):
        with patch.object(zellij, "session_list", return_value=list(sessions)):
            return zellij.attach_target("/real/zellij", args)

    def test_named_attach_alias_create_prefix_and_index(self):
        for args in [["attach", "work"], ["a", "-c", "work"], ["--config", "custom", "attach", "work"], ["--session", "work"], ["-swork"]]:
            self.assertEqual(self.target(args), ("work", args))
        self.assertEqual(self.target(["attach", "wor"])[0], "work")
        self.assertEqual(self.target(["attach"])[0], "work")
        self.assertEqual(self.target(["attach", "--index", "1"], ["one", "two"])[0], "two")
        self.assertIsNone(self.target(["attach", "--index", "-1"])[0])

    def test_new_sessions_have_known_names_and_preserve_initial_command(self):
        target, args = self.target([], [])
        self.assertTrue(target.startswith("vssh-"))
        self.assertEqual(args, ["--session", target])
        target, args = self.target(["attach", "-c", "--", "echo", "hello"], [])
        self.assertEqual(args, ["attach", "-c", target, "--", "echo", "hello"])

    def test_cli_actions_help_and_remote_clients_do_not_bind(self):
        for args in [["action", "new-pane"], ["setup", "--generate-auto-start", "zsh"], ["--help"], ["attach", "--help"], ["attach", "https://example.com/work"], ["--session", "work", "action", "list-panes"]]:
            self.assertEqual(self.target(args), (None, args))

    def test_no_connection_is_an_exact_passthrough(self):
        with patch.object(zellij, "find_real_program", return_value="/real/zellij"), patch.object(zellij, "connection", return_value={}), patch.object(zellij.os, "execv") as execute:
            with patch.object(sys, "argv", ["zellij", "attach", "work"]):
                zellij.main()
        execute.assert_called_once_with("/real/zellij", ["/real/zellij", "attach", "work"])

    def test_actions_do_not_read_or_modify_connection_state(self):
        with patch.object(zellij, "find_real_program", return_value="/real/zellij"), patch.object(zellij, "connection", side_effect=AssertionError("must not read binding")), patch.object(zellij.os, "execv") as execute:
            with patch.object(sys, "argv", ["zellij", "-s", "work", "action", "new-pane"]):
                zellij.main()
        execute.assert_called_once_with("/real/zellij", ["/real/zellij", "-s", "work", "action", "new-pane"])

    def test_attach_automatically_binds_before_exec(self):
        with patch.object(zellij, "find_real_program", return_value="/real/zellij"), patch.object(zellij, "connection", return_value=B), patch.object(zellij, "session_list", return_value=["work"]), patch.object(zellij, "bind_session") as bind, patch.object(zellij.os, "execv") as execute:
            with patch.object(sys, "argv", ["zellij", "attach", "work"]):
                zellij.main()
        bind.assert_called_once_with("work", B)
        execute.assert_called_once_with("/real/zellij", ["/real/zellij", "attach", "work"])


if __name__ == "__main__":
    unittest.main()
