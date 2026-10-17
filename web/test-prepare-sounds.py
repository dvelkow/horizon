"""Offline regression checks for pinned sound publication and startup exclusion."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


sounds = module("prepare-sounds")
assets = module("prepare-assets")


class PreparedSounds(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.resources = self.root / "resources"
        self.resources.mkdir()
        self.output = self.root / "public-assets"
        self.payload = b"verified sound object"
        self.sha1 = hashlib.sha1(self.payload).hexdigest()
        self.object = self.root / "objects" / self.sha1[:2] / self.sha1
        self.object.parent.mkdir(parents=True)
        self.object.write_bytes(self.payload)
        self.asset = "assets/minecraft/sounds/random/click.ogg"
        index = json.dumps({"objects": {self.asset.removeprefix("assets/"): {
            "hash": self.sha1, "size": len(self.payload)}}}).encode()
        self.index = self.resources / "asset-index.json"
        self.index.write_bytes(index)
        manifest = json.dumps({"id": "test", "assetIndex": {
            "sha1": hashlib.sha1(index).hexdigest(), "size": len(index)}}).encode()
        self.manifest = self.root / "manifest.json"
        self.manifest.write_bytes(manifest)
        self.lock = self.root / "lock.json"
        self.lock.write_text(json.dumps({"minecraft": {"version": "test", "manifest_sha1": hashlib.sha1(manifest).hexdigest()}}))
        self.args = ["prepare-sounds.py", "--lock", str(self.lock), "--manifest", str(self.manifest),
                     "--resources", str(self.resources), "--objects", str(self.root / "objects"),
                     "--output", str(self.output)]

    def prepare(self):
        with patch("sys.argv", self.args), patch("urllib.request.urlopen", side_effect=AssertionError("Unexpected network")), contextlib.redirect_stdout(io.StringIO()):
            sounds.main()

    def test_verified_objects_published_with_hashes_and_readable_modes(self):
        self.prepare()
        for base in (self.resources, self.output):
            path = base / self.asset
            self.assertEqual(path.read_bytes(), self.payload)
            self.assertEqual(path.stat().st_mode & 0o777, 0o644)
        index = json.loads((self.output / "browser-sounds.json").read_bytes())
        self.assertEqual(index["sounds"][self.asset], {"bytes": len(self.payload), "sha256": hashlib.sha256(self.payload).hexdigest()})

    def test_corrupt_source_object_does_not_publish_a_sound_manifest(self):
        self.object.write_bytes(b"x" * len(self.payload))
        with self.assertRaisesRegex(ValueError, "Missing/corrupt"):
            self.prepare()
        self.assertFalse((self.output / "browser-sounds.json").exists())

    def test_unpinned_manifest_and_asset_index_are_rejected(self):
        original = self.manifest.read_bytes()
        self.manifest.write_bytes(original + b" ")
        with self.assertRaisesRegex(ValueError, "research-lock"):
            self.prepare()
        self.manifest.write_bytes(original)
        self.index.write_bytes(self.index.read_bytes() + b" ")
        with self.assertRaisesRegex(ValueError, "Asset index"):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_startup_bundle_keeps_sound_index_but_excludes_raw_audio(self):
        self.prepare()
        (self.resources / "horizon-resources.json").write_text(json.dumps({"minecraft": "test", "data_version": 1, "schema": 1}))
        with patch("sys.argv", ["prepare-assets.py", "--resources", str(self.resources), "--output", str(self.output)]), contextlib.redirect_stdout(io.StringIO()):
            assets.main()
        pointer = json.loads((self.output / "index.json").read_bytes())
        index = json.loads((self.output / Path(pointer["url"]).name).read_bytes())
        names = [entry["path"] for bundle in index["bundles"] for entry in bundle["files"]]
        self.assertIn("browser-sounds.json", names)
        self.assertNotIn(self.asset, names)
        self.assertEqual((self.output / self.asset).read_bytes(), self.payload)


if __name__ == "__main__":
    unittest.main()
