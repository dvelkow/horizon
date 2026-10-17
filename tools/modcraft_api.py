#!/usr/bin/env python3
"""Submit locally built Modcraft mods and review source without opening the workshop."""

import argparse
import base64
import json
import os
from pathlib import Path
import stat
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # Never forward credentials to a redirect destination.


def read_private(path):
    path = Path(path)
    if path.is_symlink() or not stat.S_ISREG(path.stat().st_mode) or path.stat().st_mode & 0o077:
        raise ValueError("Credential files must be regular private files (chmod 600)")
    return path.read_text(encoding="utf-8").strip()


def origin_url(value):
    url = urllib.parse.urlsplit(value)
    if url.scheme != "https" or not url.hostname or url.username or url.password or url.path not in ("", "/") or url.query or url.fragment:
        raise ValueError("Use an HTTPS site origin without credentials, query, or path")
    return f"https://{url.netloc}"


def request(origin, path, token=None, payload=None, headers=None):
    values = {"Accept": "application/json", **(headers or {})}
    if token:
        values["Authorization"] = "Bearer " + token
    data = None
    if payload is not None:
        values["Content-Type"] = "application/json"
        data = json.dumps(payload).encode()
    req = urllib.request.Request(origin + path, data=data, headers=values)
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=120) as response:
            return json.load(response), response.headers
    except urllib.error.HTTPError as error:
        try:
            value = json.load(error)
        except (ValueError, UnicodeError):
            value = {"error": "Request failed"}
        # A rejected submission includes its hash, checks and polling URL.
        if error.code == 422 and value.get("sha256"):
            return value, error.headers
        raise ValueError(f"HTTP {error.code}: {value.get('error', 'Request failed')}") from None


def source_files(project, extras=()):
    root = Path(project).resolve(strict=True)
    paths = {Path(name) for name in ("Cargo.toml", "Cargo.lock", "horizon-mod.toml", "build.rs") if (root / name).exists()}
    for folder, extension in (("src", "*.rs"), ("wit", "*.wit")):
        paths.update(path.relative_to(root) for path in (root / folder).rglob(extension))
    paths.update(Path(path) for path in extras)
    if not 1 <= len(paths) <= 64:
        raise ValueError("Include 1–64 source files")
    result, total = [], 0
    for path in sorted(paths):
        if path.is_absolute() or any(part.startswith(".") for part in path.parts):
            raise ValueError("Source paths must be relative, without hidden files or traversal")
        file = root / path
        if not file.resolve(strict=True).is_relative_to(root) or any((root / Path(*path.parts[:i])).is_symlink() for i in range(1, len(path.parts) + 1)):
            raise ValueError("Source files cannot be symlinks or outside the mod project")
        if not file.is_file() or file.stat().st_size > 512 * 1024 - total:
            raise ValueError("Source files must fit within 512 KB")
        content = file.read_text(encoding="utf-8")
        total += len(content.encode())
        result.append({"path": path.as_posix(), "content": content})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=os.environ.get("MODCRAFT_API_CONFIG", str(Path.home() / ".config/modcraft/agent.json")), help="Private agent credential JSON")
    commands = parser.add_subparsers(dest="command", required=True)
    submit = commands.add_parser("submit", help="Send a compiled .hmod, source, and review notes")
    submit.add_argument("--package", required=True)
    submit.add_argument("--project", required=True, help="Mod source directory; Rust source, WIT and manifests are included")
    submit.add_argument("--notes", required=True)
    submit.add_argument("--source", action="append", default=[], help="Additional relative text source path, repeatable")
    commands.add_parser("list", help="List this key's submissions")
    status = commands.add_parser("status", help="Read one submission's status")
    status.add_argument("sha256")
    create = commands.add_parser("key-create", help="Host only: issue a submission key without the workshop")
    create.add_argument("--origin", required=True)
    create.add_argument("--owner-key-file", required=True, help="Private file containing the host review key, not a cloud credential")
    create.add_argument("--label", required=True)
    create.add_argument("--days", type=int, default=30)
    create.add_argument("--out", required=True, help="New private credential JSON; never overwritten")
    for name in ("key-list", "key-revoke"):
        command = commands.add_parser(name, help="Host only: manage submission keys")
        command.add_argument("--origin", required=True)
        command.add_argument("--owner-key-file", required=True)
        if name == "key-revoke":
            command.add_argument("id")
    args = parser.parse_args()
    if args.command.startswith("key-"):
        origin = origin_url(args.origin)
        if args.command == "key-create" and Path(args.out).exists():
            raise ValueError("Output credential file already exists; choose a new path")
        session, headers = request(origin, "/api/admin/login", payload={"secret": read_private(args.owner_key_file)}, headers={"Origin": origin})
        auth = {"Origin": origin, "Cookie": headers["Set-Cookie"].split(";")[0], "X-CSRF-Token": session["csrf"]}
        try:
            if args.command == "key-create":
                output = Path(args.out)
                output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                # Reserve the private destination before issuing a live key.
                with open(output, "x", encoding="utf-8", opener=lambda path, flags: os.open(path, flags, 0o600)) as file:
                    key, _ = request(origin, "/api/admin/mod-api-tokens", payload={"label": args.label, "days": args.days}, headers=auth)
                    json.dump({"origin": origin, "token": key["token"], "id": key["id"], "expires": key["expires"]}, file, indent=2)
                    file.write("\n")
                print(json.dumps({"credentialFile": str(output.resolve()), "id": key["id"], "expires": key["expires"], "scopes": key["scopes"]}, indent=2))
            else:
                path = "/api/admin/mod-api-tokens"
                payload = None
                if args.command == "key-revoke":
                    if len(args.id) != 16 or any(c not in "0123456789abcdef" for c in args.id):
                        raise ValueError("Invalid key ID")
                    path += f"/{args.id}/revoke"
                    payload = {}
                result, _ = request(origin, path, payload=payload, headers=auth)
                print(json.dumps(result, indent=2))
        finally:
            request(origin, "/api/admin/logout", payload={}, headers=auth)
        return 0
    config = json.loads(read_private(args.config))
    origin = origin_url(config["origin"])
    token = config["token"]
    path, payload = "/api/v1/mods/submissions", None
    if args.command == "submit":
        package = Path(args.package)
        if package.suffix != ".hmod" or not 0 < package.stat().st_size <= 16 * 1024 * 1024:
            raise ValueError("Choose a nonempty .hmod file up to 16 MB")
        payload = {"packageBase64": base64.b64encode(package.read_bytes()).decode(), "notes": args.notes, "files": source_files(args.project, args.source)}
    elif args.command == "status":
        if len(args.sha256) != 64 or any(c not in "0123456789abcdef" for c in args.sha256):
            raise ValueError("Invalid package SHA-256")
        path += "/" + args.sha256
    result, _ = request(origin, path, token=token, payload=payload)
    print(json.dumps(result, indent=2))
    return 1 if result.get("status") == "rejected" else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, KeyError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
