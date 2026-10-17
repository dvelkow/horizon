import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("modcraft_api", Path(__file__).resolve().parents[1] / "tools/modcraft_api.py")
api = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(api)


class ModcraftClientTests(unittest.TestCase):
    def test_source_selection_excludes_private_files_and_build_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("// mod")
            (root / "Cargo.toml").write_text("[package]")
            (root / ".env").write_text("secret")
            (root / "target").mkdir()
            (root / "target/unrelated.rs").write_text("not mod source")
            self.assertEqual([f["path"] for f in api.source_files(root)], ["Cargo.toml", "src/lib.rs"])
            for name in (".env", "../outside", str(root / "Cargo.toml")):
                with self.assertRaises(ValueError):
                    api.source_files(root, [name])
            (root / "src/link.rs").symlink_to(root / ".env")
            with self.assertRaises(ValueError):
                api.source_files(root)

    def test_credentials_require_private_permissions_and_https(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "agent.json"
            path.write_text("secret")
            os.chmod(path, 0o644)
            with self.assertRaises(ValueError):
                api.read_private(path)
            os.chmod(path, 0o600)
            self.assertEqual(api.read_private(path), "secret")
        for url in ("http://game.test", "https://user:secret@game.test", "https://game.test/?token=secret", "https://game.test/path"):
            with self.assertRaises(ValueError):
                api.origin_url(url)
        self.assertEqual(api.origin_url("https://game.test/"), "https://game.test")

    def test_key_creation_saves_secret_once_and_never_prints_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            owner = root / "owner-key"
            owner.write_text("host-secret")
            os.chmod(owner, 0o600)
            destination = root / "agent.json"
            key = {"id": "a" * 16, "token": "private-test-token", "expires": 123, "scopes": ["mods:submit", "mods:read-own"]}
            responses = [({"csrf": "csrf"}, {"Set-Cookie": "session=value; Path=/"}), (key, {}), ({"ok": True}, {})]
            argv = ["modcraft_api.py", "key-create", "--origin", "https://game.test", "--owner-key-file", str(owner), "--label", "Test", "--out", str(destination)]
            output = io.StringIO()
            with patch("sys.argv", argv), patch.object(api, "request", side_effect=responses), contextlib.redirect_stdout(output):
                self.assertEqual(api.main(), 0)
            self.assertNotIn(key["token"], output.getvalue())
            self.assertEqual(json.loads(destination.read_text())["token"], key["token"])
            self.assertEqual(destination.stat().st_mode & 0o777, 0o600)
            with patch("sys.argv", argv), patch.object(api, "request") as request:
                with self.assertRaises(ValueError):
                    api.main()
                request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
