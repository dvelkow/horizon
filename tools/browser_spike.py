#!/usr/bin/env python3
"""Read-only browser-port inventory; --build explicitly runs the real wasm build.

Requires Python 3.11+. This is a lexical inventory, not a Rust compiler, feature
resolver, browser smoke test, or claim that Horizon supports the browser.
"""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib


LITERALS = {
    "std_thread": "std::thread",
    "std_fs": "std::fs",
    "instant_now": "Instant::now",
}
BUILD = [
    "cargo", "build", "--locked", "-p", "horizon-client", "--bin",
    "horizon-client", "--target", "wasm32-unknown-unknown",
]


def display(path, root):
    try:
        return str(path.relative_to(root))
    except ValueError:
        return str(path)


def resolve(path, root):
    path = Path(path)
    return (path if path.is_absolute() else root / path).resolve()


def split_use(text):
    """Split a Rust use tree's children without splitting nested groups."""
    depth, start = 0, 0
    for i, char in enumerate(text):
        depth += (char == "{") - (char == "}")
        if char == "," and depth == 0:
            yield text[start:i].strip()
            start = i + 1
    yield text[start:].strip()


def use_names(text, prefix=("std",)):
    """Recognize common std imports, including groups, self and as aliases."""
    text = text.strip()
    if "{" in text:
        head, children = text.split("{", 1)
        parent = prefix + tuple(p for p in head.strip(" :").split("::") if p)
        for child in split_use(children.rsplit("}", 1)[0]):
            yield from use_names(child, parent)
    elif text and "*" not in text:
        parts = re.split(r"\s+as\s+", text, maxsplit=1)
        tail = tuple(p.strip() for p in parts[0].split("::"))
        path = prefix + (tail[:-1] if tail[-1] == "self" else tail)
        alias = parts[1].strip() if len(parts) == 2 else path[-1]
        if re.fullmatch(r"[A-Za-z_]\w*", alias):
            yield "::".join(path), alias


def lines_for(text, pattern):
    return {text.count("\n", 0, match.start()) + 1 for match in re.finditer(pattern, text)}


def source_inventory(paths, root):
    files = sorted({file for path in paths for file in path.rglob("*.rs")})
    literal = {key: [] for key in LITERALS}
    aliases = {key: [] for key in LITERALS}
    for file in files:
        text = file.read_text(encoding="utf-8")
        found = {key: lines_for(text, re.escape(value)) for key, value in LITERALS.items()}
        for key, lines in found.items():
            if lines:
                literal[key].append({"file": display(file, root), "lines": sorted(lines)})
        for statement in re.finditer(r"\buse\s+(?:::)?std\s*::\s*([^;]+);", text):
            for path, alias in use_names(statement.group(1)):
                key = next((key for key, prefix in (("std_thread", "std::thread"),
                                                    ("std_fs", "std::fs"))
                            if path == prefix or path.startswith(prefix + "::")), None)
                if key:
                    found[key].add(text.count("\n", 0, statement.start()) + 1)
                    found[key].update(lines_for(text, rf"\b{re.escape(alias)}\s*(?:::|\()"))
                elif path == "std::time::Instant":
                    found["instant_now"].update(lines_for(text, rf"\b{re.escape(alias)}\s*::\s*now\s*\("))
        for key, lines in found.items():
            if lines:
                aliases[key].append({"file": display(file, root), "lines": sorted(lines)})

    def summarize(groups):
        return {key: {"file_count": len(rows),
                      "matched_line_count": sum(len(row["lines"]) for row in rows),
                      "files": rows} for key, rows in groups.items()}

    return {
        "roots": [{"path": display(path, root), "exists": path.is_dir()} for path in paths],
        "rust_file_count": len(files),
        "literal": summarize(literal),
        "alias_aware_best_effort": summarize(aliases),
        "limitations": [
            "Raw source includes tests, comments, strings and cfg-disabled code.",
            "Alias scan handles common std use trees; it does not resolve scopes, globs, re-exports or macros.",
            "Counts identify review locations, not confirmed browser blockers or all transitive dependencies.",
        ],
    }


def manifests_inventory(root, initial):
    """Read selected manifests and their local path dependencies, without Cargo."""
    pending, seen, output = list(initial), set(), []
    while pending:
        path = pending.pop().resolve()
        if path in seen:
            continue
        seen.add(path)
        if not path.is_file():
            output.append({"path": display(path, root), "exists": False})
            continue
        with path.open("rb") as stream:
            manifest = tomllib.load(stream)
        rows = []
        tables = [("all", manifest), ("workspace", manifest.get("workspace", {}))]
        tables += list(manifest.get("target", {}).items())
        for target, table in tables:
            for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
                for name, value in sorted(table.get(kind, {}).items()):
                    spec = {"version": value} if isinstance(value, str) else value
                    row = {"name": name, "kind": kind, "target": target}
                    # Deliberately exclude registry/git URLs and other unrelated metadata.
                    row.update({key: spec[key] for key in
                                ("version", "features", "optional", "package", "workspace") if key in spec})
                    row["default_features"] = spec.get("default-features", True)
                    if "path" in spec:
                        dependency = resolve(spec["path"], path.parent)
                        row["path"] = display(dependency, root)
                        pending.append(dependency / "Cargo.toml")
                    rows.append(row)
        package = manifest.get("package", {})
        output.append({
            "path": display(path, root), "exists": True,
            "package": package.get("name"), "version": package.get("version"),
            "features": manifest.get("features", {}), "dependencies": rows,
        })
    return {"manifests": sorted(output, key=lambda item: item["path"]),
            "limitations": "Declared flags only; target cfg expressions and workspace feature unification are not evaluated."}


def assets_inventory(paths, root):
    markers = ("horizon-resources.json", "facts.json", "block-behaviours.json", "assets")
    output = []
    for path in paths:
        children = sorted(child for child in path.iterdir() if child.is_dir()) if path.is_dir() else []
        candidates = [path] + [child for child in children if (child / markers[0]).is_file()]
        output.append({
            "path": display(path, root), "exists": path.is_dir(),
            "resource_candidates": [{"path": display(candidate, root),
                                     "markers": {name: (candidate / name).exists() for name in markers}}
                                    for candidate in candidates],
        })
    return {"paths": output, "limitations": "Presence only; files are not validated and resources are not prepared or downloaded."}


def build(root, directory):
    directory.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    log = directory / f"wasm-build-{stamp}.log"
    with log.open("x", encoding="utf-8") as stream:
        try:
            result = subprocess.run(BUILD, cwd=root, stdout=stream, stderr=subprocess.STDOUT, check=False)
            code = result.returncode
        except OSError as error:
            stream.write(f"Unable to start cargo: {error}\n")
            code = 127
    return {"command": BUILD, "returncode": code, "log": display(log, root),
            "validation": "Compilation only; success does not prove a browser title screen or gameplay works."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--source-root", action="append", help="Rust source root relative to repo; repeatable")
    parser.add_argument("--manifest", action="append", help="Cargo manifest relative to repo; repeatable")
    parser.add_argument("--assets", action="append", help="Prepared resource directory or version parent; repeatable")
    parser.add_argument("--build", action="store_true", help="Run Cargo; may download dependencies and write build outputs")
    parser.add_argument("--output-dir", default=".local/browser-spike", help="With --build, directory for build log and JSON report")
    args = parser.parse_args()
    root = args.repo.resolve()
    sources = [resolve(path, root) for path in (args.source_root or ["crates/horizon-client/src"])]
    manifests = [resolve(path, root) for path in (args.manifest or ["Cargo.toml", "crates/horizon-client/Cargo.toml"])]
    assets = [resolve(path, root) for path in (args.assets or [".local/horizon-client"])]
    try:
        revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root, check=False,
                                  capture_output=True, text=True)
        commit = revision.stdout.strip() if revision.returncode == 0 else None
    except OSError:
        commit = None
    report = {
        "schema_version": 1, "git_revision": commit,
        "source": source_inventory(sources, root),
        "dependencies": manifests_inventory(root, manifests),
        "assets": assets_inventory(assets, root),
        "build": {"requested": False},
    }
    code = 0
    if args.build:
        directory = resolve(args.output_dir, root)
        report["build"] = {"requested": True, **build(root, directory)}
        code = report["build"]["returncode"]
        report_path = directory / (Path(report["build"]["log"]).stem + ".json")
        report["build"]["report"] = display(report_path, root)
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return code if code >= 0 else 128 - code


if __name__ == "__main__":
    sys.exit(main())
