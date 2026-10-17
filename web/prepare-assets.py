#!/usr/bin/env python3
"""Package prepared Horizon resources into deterministic, hashed browser bundles.

Outputs contain Minecraft resources: keep the destination outside source control
and serve it only through the intended game asset service.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import os
import tempfile
import zipfile


def json_bytes(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def atomic_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".bundle-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
        Path(temporary).replace(path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resources", type=Path, default=Path(".local/horizon-client/26.3"))
    parser.add_argument("--output", type=Path, default=Path(".local/web/assets"))
    parser.add_argument("--bundle-mib", type=int, default=8)
    args = parser.parse_args()
    if not 1 <= args.bundle_mib <= 64:
        parser.error("--bundle-mib must be 1..64")
    source = args.resources.resolve()
    output = args.output.resolve()
    if output.is_relative_to(source):
        parser.error("output must be outside the resources directory")
    manifest = json.loads((source / "horizon-resources.json").read_text())
    files = []
    for path in sorted(source.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"Resource symlink is not permitted: {path}")
        if not path.is_file():
            continue
        relative = path.relative_to(source)
        if relative.parts[0] in {"classes", "client-classes"} or path.suffix in {".class", ".java", ".log"}:
            continue
        if path.stat().st_size > 128 * 1024 * 1024:
            raise ValueError(f"Resource exceeds file size cap: {relative}")
        files.append((relative.as_posix(), path))
    output.mkdir(parents=True, exist_ok=True)
    index = {"schema": 1, "minecraft": manifest["minecraft"], "data_version": manifest["data_version"],
             "resource_schema": manifest["schema"], "bundles": [], "file_count": 0, "expanded_bytes": 0}
    pending, pending_size = [], 0

    def bundle(rows):
        memory = io.BytesIO()
        entries = []
        with zipfile.ZipFile(memory, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for name, path in rows:
                data = path.read_bytes()
                entry = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = 0o100644 << 16
                archive.writestr(entry, data, compresslevel=6)
                entries.append({"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
        compressed = memory.getvalue()
        sha256 = hashlib.sha256(compressed).hexdigest()
        filename = sha256 + ".zip"
        atomic_write(output / filename, compressed)
        expanded = sum(e["bytes"] for e in entries)
        index["bundles"].append({"url": "/assets/" + filename, "sha256": sha256,
                                 "bytes": len(compressed), "expanded_bytes": expanded, "files": entries})
        index["file_count"] += len(entries)
        index["expanded_bytes"] += expanded
        print(f"Bundle {len(index['bundles'])}: {len(entries)} files, {len(compressed):,} compressed bytes", flush=True)

    for row in files:
        size = row[1].stat().st_size
        if pending and pending_size + size > args.bundle_mib * 1024 * 1024:
            bundle(pending)
            pending, pending_size = [], 0
        pending.append(row)
        pending_size += size
    if pending:
        bundle(pending)
    encoded = json_bytes(index)
    sha256 = hashlib.sha256(encoded).hexdigest()
    atomic_write(output / (sha256 + ".json"), encoded)
    pointer = {"schema": 1, "url": "/assets/" + sha256 + ".json", "sha256": sha256, "bytes": len(encoded)}
    atomic_write(output / "index.json", json_bytes(pointer))
    print(json.dumps({"output": str(output), "files": index["file_count"], "expanded_bytes": index["expanded_bytes"],
                      "download_bytes": sum(b["bytes"] for b in index["bundles"]), "bundles": len(index["bundles"]),
                      "index_sha256": sha256}, indent=2))


if __name__ == "__main__":
    main()
