#!/usr/bin/env python3
"""Verify pinned Minecraft sounds, prepare native files, and publish on-demand audio.

Network access is opt-in with --download. Every source object is checked against
the SHA-1 and size in the pinned official asset index. The browser receives a
small SHA-256 manifest; the Ogg files stay outside its startup ZIP bundles.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import tempfile
import time
import urllib.request


def atomic_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".sound-", dir=path.parent)
    try:
        os.fchmod(fd, 0o644)
        with os.fdopen(fd, "wb") as output:
            output.write(data)
        Path(temporary).replace(path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def checked(path, checksum, size):
    if not path.is_file() or path.stat().st_size != size:
        return None
    data = path.read_bytes()
    return data if hashlib.sha1(data).hexdigest() == checksum else None


def fetch_object(store, checksum, size, download):
    target = store / checksum[:2] / checksum
    data = checked(target, checksum, size)
    if data is None:
        if not download:
            raise ValueError(f"Missing/corrupt object {checksum}; use --download to fetch it")
        url = f"https://resources.download.minecraft.net/{checksum[:2]}/{checksum}"
        for attempt in range(4):
            try:
                with urllib.request.urlopen(url, timeout=45) as response:
                    if response.geturl() != url:
                        raise ValueError("Unexpected asset download redirect")
                    data = response.read(size + 1)
                if len(data) != size or hashlib.sha1(data).hexdigest() != checksum:
                    raise ValueError(f"Downloaded object failed verification: {checksum}")
                atomic_write(target, data)
                break
            except (OSError, ValueError):
                if attempt == 3:
                    raise
                time.sleep(0.5 * 2**attempt)
    return checksum, data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, default=Path("research-lock.json"))
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--index", type=Path)
    parser.add_argument("--resources", type=Path)
    parser.add_argument("--objects", type=Path)
    parser.add_argument("--output", type=Path, default=Path(".local/web/assets"))
    parser.add_argument("--download", action="store_true")
    parser.add_argument("--jobs", type=int, default=8)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 16:
        parser.error("--jobs must be 1..16")
    lock = json.loads(args.lock.read_bytes())["minecraft"]
    version = lock["version"]
    manifest_path = args.manifest or Path(f".local/minecraft/{version}/manifest.json")
    resources = args.resources or Path(f".local/horizon-client/{version}")
    object_store = args.objects or Path(f".local/minecraft-{version}/assets/objects")
    raw_manifest = manifest_path.read_bytes()
    allowed = [lock["manifest_sha1"], *lock.get("accepted_manifest_sha1", [])]
    if hashlib.sha1(raw_manifest).hexdigest() not in allowed:
        raise ValueError("Version manifest does not match research-lock.json")
    manifest = json.loads(raw_manifest)
    if manifest["id"] != version:
        raise ValueError("Version manifest has the wrong Minecraft version")
    index_path = args.index or resources / "asset-index.json"
    raw_index = index_path.read_bytes()
    info = manifest["assetIndex"]
    if len(raw_index) != info["size"] or hashlib.sha1(raw_index).hexdigest() != info["sha1"]:
        raise ValueError("Asset index does not match the pinned version manifest")
    sounds, objects = {}, {}
    for name, item in json.loads(raw_index)["objects"].items():
        if name != "minecraft/sounds.json" and not name.startswith("minecraft/sounds/"):
            continue
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or "\\" in name:
            raise ValueError(f"Unsafe asset path: {name}")
        checksum, size = item["hash"], item["size"]
        if not re.fullmatch("[0-9a-f]{40}", checksum) or not 0 < size <= 32 << 20:
            raise ValueError(f"Invalid asset metadata: {name}")
        sounds.setdefault(checksum, []).append(name)
        if checksum in objects and objects[checksum] != size:
            raise ValueError("Conflicting sizes for one asset object")
        objects[checksum] = size
    if not sounds:
        raise ValueError("Pinned asset index contains no sounds")
    browser = {"schema": 1, "minecraft": version, "asset_index_sha1": info["sha1"], "sounds": {}}
    print(f"Verifying {sum(map(len, sounds.values()))} sounds ({sum(objects.values()):,} object bytes)", flush=True)
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        pending = [pool.submit(fetch_object, object_store, checksum, size, args.download)
                   for checksum, size in objects.items()]
        for count, future in enumerate(as_completed(pending), 1):
            checksum, data = future.result()
            for name in sounds[checksum]:
                relative = "assets/" + name
                for base in (resources, args.output):
                    destination = base / relative
                    if checked(destination, checksum, len(data)) is None:
                        atomic_write(destination, data)
                    destination.chmod(0o644)
                if name.startswith("minecraft/sounds/"):
                    browser["sounds"][relative] = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            if count % 500 == 0 or count == len(pending):
                print(f"Verified and prepared {count}/{len(pending)} objects", flush=True)
    encoded = (json.dumps(browser, sort_keys=True, separators=(",", ":")) + "\n").encode()
    # The native resources copy becomes part of the small hashed startup bundle.
    atomic_write(resources / "browser-sounds.json", encoded)
    atomic_write(args.output / "browser-sounds.json", encoded)
    print(json.dumps({"minecraft": version, "sounds": len(browser["sounds"]),
                      "manifest_sha256": hashlib.sha256(encoded).hexdigest(),
                      "manifest_bytes": len(encoded), "raw_sound_bytes": sum(x["bytes"] for x in browser["sounds"].values()),
                      "resources": str(resources), "output": str(args.output)}, indent=2))


if __name__ == "__main__":
    main()
