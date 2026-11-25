"""Sign, verify, activate and roll back portable NexDesk builds."""

import argparse
import hashlib
import json
import os
import platform
import re
import sqlite3
import stat
import subprocess
import tempfile
import zipfile
from contextlib import closing
from pathlib import Path, PurePosixPath

MAX_BYTES = 4 * 1024**3
MAX_FILES = 10_000
MAX_MANIFEST = 3 * 1024**2
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9]+(?:[.-][a-zA-Z0-9]+)*)?")
DIGEST = re.compile(r"[0-9a-f]{64}")
TARGETS = ("windows-x64", "windows-arm64", "linux-x64", "linux-arm64", "macos-x64", "macos-arm64")


def relative_name(value):
    if not isinstance(value, str) or len(value) > 240:
        raise ValueError("invalid file name")
    for part in value.split("/"):
        if (
            not part or part in (".", "..") or part.endswith((".", " "))
            or re.search(r'[\x00-\x1f<>:"\\|?*]', part)
            or re.fullmatch(r"(?i:CON|PRN|AUX|NUL|COM[1-9¹²³]|LPT[1-9¹²³])", part.split(".")[0])
        ):
            raise ValueError("unsafe portable file name")
    return value


def validate(manifest):
    if (
        manifest["format"] != 1
        or not isinstance(manifest["version"], str) or len(manifest["version"]) > 60
        or not VERSION.fullmatch(manifest["version"])
        or manifest["target"] not in TARGETS
        or type(manifest["protocol"]) is not int or not 1 <= manifest["protocol"] <= 65535
    ):
        raise ValueError("unsupported release manifest")
    entries = manifest["files"]
    if not isinstance(entries, dict) or not 1 <= len(entries) <= MAX_FILES:
        raise ValueError("invalid file count")
    names = set()
    size = 0
    for name, entry in entries.items():
        folded = relative_name(name).casefold()
        if folded in names:
            raise ValueError("duplicate portable file name")
        names.add(folded)
        if (
            not DIGEST.fullmatch(entry["sha256"])
            or type(entry["size"]) is not int or not 0 <= entry["size"] <= MAX_BYTES
            or type(entry["executable"]) is not bool
        ):
            raise ValueError("invalid file metadata")
        size += entry["size"]
    for name in names:
        if any(str(parent) in names for parent in PurePosixPath(name).parents):
            raise ValueError("a file cannot also be a directory")
    if size > MAX_BYTES or relative_name(manifest["entrypoint"]) not in entries:
        raise ValueError("invalid bundle size or entry point")
    return manifest


def openssl(*args):
    return subprocess.run(
        ("openssl", *args), check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    ).stdout


def authenticate(data, signature, key):
    if len(data) > MAX_MANIFEST or len(signature) != 64:
        raise ValueError("invalid manifest or signature length")
    public = openssl("pkey", "-pubin", "-in", key, "-outform", "DER")
    if len(public) != 44 or not public.startswith(bytes.fromhex("302a300506032b6570032100")):
        raise ValueError("an Ed25519 release public key is required")
    with tempfile.TemporaryDirectory(prefix="nexdesk-signature-") as scratch:
        blob = Path(scratch, "release.json")
        proof = Path(scratch, "release.sig")
        blob.write_bytes(data)
        proof.write_bytes(signature)
        openssl("pkeyutl", "-verify", "-pubin", "-inkey", key, "-rawin", "-in", blob, "-sigfile", proof)
    return validate(json.loads(data))


def files_at(root):
    if not root.is_dir():
        raise ValueError("the payload directory is missing")
    result = {}
    for path in root.rglob("*"):
        if path.is_symlink() or path.is_junction():
            raise ValueError("portable bundles cannot contain links")
        if path.is_dir():
            continue
        if not path.is_file() or len(result) >= MAX_FILES:
            raise ValueError("unexpected file type or count")
        result[relative_name(path.relative_to(root).as_posix())] = path
    return result


def copy_checked(source, entry, destination=None):
    digest = hashlib.sha256()
    count = 0
    while chunk := source.read(1024**2):
        count += len(chunk)
        if count > entry["size"]:
            raise ValueError("file exceeds signed length")
        digest.update(chunk)
        if destination is not None:
            destination.write(chunk)
    if count != entry["size"] or digest.hexdigest() != entry["sha256"]:
        raise ValueError("file differs from signed manifest")


def package(args):
    root = args.payload.resolve(strict=True)
    output = args.output.resolve()
    key = args.key.resolve(strict=True)
    if output.is_relative_to(root) or key.is_relative_to(root):
        raise ValueError("keep the signing key and output outside the payload")
    sources = files_at(root)
    files = {}
    for name, path in sources.items():
        info = path.stat()
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256")
        files[name] = {
            "size": info.st_size,
            "sha256": digest.hexdigest(),
            "executable": bool(info.st_mode & stat.S_IXUSR),
        }
    manifest = validate({
        "format": 1, "version": args.version, "target": args.target,
        "protocol": args.protocol, "entrypoint": args.entrypoint, "files": files,
    })
    encoded = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()
    if len(encoded) > MAX_MANIFEST or output.exists():
        raise ValueError("manifest is too large or output already exists")
    with tempfile.TemporaryDirectory(prefix=".nexdesk-release-", dir=output.parent) as scratch:
        blob = Path(scratch, "release.json")
        blob.write_bytes(encoded)
        signature = openssl("pkeyutl", "-sign", "-inkey", key, "-rawin", "-in", blob)
        if len(signature) != 64:
            raise ValueError("an Ed25519 signing key is required")
        staged = Path(scratch, "bundle.zip")
        with zipfile.ZipFile(staged, "x", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr("release.json", encoded)
            archive.writestr("release.sig", signature)
            for name, path in sorted(sources.items()):
                with path.open("rb") as stream, archive.open(f"payload/{name}", "w", force_zip64=True) as entry:
                    copy_checked(stream, files[name], entry)
        os.link(staged, output)


def signed_archive(archive, key):
    infos = archive.infolist()
    headers = {}
    for info in infos:
        if (
            len(infos) > MAX_FILES + 2 or info.filename in headers
            or stat.S_IFMT(info.external_attr >> 16) not in (0, stat.S_IFREG)
        ):
            raise ValueError("unexpected or duplicate archive entry")
        headers[info.filename] = info
    if headers["release.json"].file_size > MAX_MANIFEST or headers["release.sig"].file_size != 64:
        raise ValueError("invalid archive metadata length")
    data = archive.read("release.json")
    signature = archive.read("release.sig")
    manifest = authenticate(data, signature, key)
    if set(headers) != {"release.json", "release.sig", *(f"payload/{name}" for name in manifest["files"])}:
        raise ValueError("archive and signed file list differ")
    for name, entry in manifest["files"].items():
        if headers[f"payload/{name}"].file_size != entry["size"]:
            raise ValueError("archive and signed file lengths differ")
    return data, signature, manifest


def read_bundle(archive, manifest, destination=None):
    for name, entry in manifest["files"].items():
        with archive.open(f"payload/{name}") as source:
            if destination is None:
                copy_checked(source, entry)
                continue
            output = destination / name
            output.parent.mkdir(parents=True, exist_ok=True)
            with output.open("xb") as stream:
                copy_checked(source, entry, stream)
            output.chmod(0o755 if entry["executable"] else 0o644)


def current_target():
    cpu = {"amd64": "x64", "x86_64": "x64", "aarch64": "arm64", "arm64": "arm64"}
    system = {"Windows": "windows", "Linux": "linux", "Darwin": "macos"}
    return f"{system[platform.system()]}-{cpu[platform.machine().lower()]}"


def version_id(data):
    return hashlib.sha256(data).hexdigest()


def installed(root, name, key):
    if not DIGEST.fullmatch(name):
        raise ValueError("invalid activation state")
    version = root / "versions" / name
    if version.resolve() != version:
        raise ValueError("unexpected version path")
    manifest_path = version / "release.json"
    if manifest_path.stat().st_size > MAX_MANIFEST or (version / "release.sig").stat().st_size != 64:
        raise ValueError("invalid installed manifest length")
    data = manifest_path.read_bytes()
    manifest = authenticate(data, (version / "release.sig").read_bytes(), key)
    if version_id(data) != name or manifest["target"] != current_target():
        raise ValueError("installed version or target mismatch")
    payload = version / "payload"
    if payload.resolve() != payload:
        raise ValueError("unexpected payload path")
    paths = files_at(payload)
    if paths.keys() != manifest["files"].keys():
        raise ValueError("installed files differ from the signed list")
    for path, entry in manifest["files"].items():
        with paths[path].open("rb") as stream:
            copy_checked(stream, entry)
    return payload / manifest["entrypoint"]


def activate(args):
    root = args.root.resolve()
    versions = root / "versions"
    versions.mkdir(parents=True, exist_ok=True)
    if versions.resolve() != versions:
        raise ValueError("unexpected versions directory")
    database = root / "activation.sqlite3"
    if database.resolve() != database:
        raise ValueError("unexpected activation database path")
    with closing(sqlite3.connect(database, timeout=15)) as db, db:
        db.execute("BEGIN IMMEDIATE")
        db.execute("CREATE TABLE IF NOT EXISTS active (id INTEGER PRIMARY KEY, current TEXT, previous TEXT)")
        current, previous = db.execute("SELECT current, previous FROM active WHERE id=1").fetchone() or (None, None)
        if args.command == "launch":
            if current is None:
                raise ValueError("no active release")
            entrypoint = installed(root, current, args.key)
        elif args.command == "rollback":
            if previous is None:
                raise ValueError("no previous release")
            installed(root, previous, args.key)
            db.execute("UPDATE active SET current=?, previous=? WHERE id=1", (previous, current))
        else:
            with zipfile.ZipFile(args.bundle) as archive:
                data, signature, manifest = signed_archive(archive, args.key)
                if manifest["target"] != current_target():
                    raise ValueError("release is for a different platform")
                name = version_id(data)
                version = versions / name
                if not version.exists():
                    with tempfile.TemporaryDirectory(prefix=".nexdesk-stage-", dir=versions) as scratch:
                        stage = Path(scratch, "version")
                        (stage / "payload").mkdir(parents=True)
                        read_bundle(archive, manifest, stage / "payload")
                        (stage / "release.json").write_bytes(data)
                        (stage / "release.sig").write_bytes(signature)
                        if stage.resolve().parent.parent != versions or version.resolve().parent != versions:
                            raise ValueError("unexpected staging path")
                        stage.rename(version)
                else:
                    read_bundle(archive, manifest)
                    installed(root, name, args.key)
                if name != current:
                    db.execute("INSERT OR REPLACE INTO active VALUES (1,?,?)", (name, current))
    if args.command == "launch":
        return subprocess.run([entrypoint], cwd=entrypoint.parent, check=False).returncode
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("package")
    make.add_argument("payload", type=Path)
    make.add_argument("output", type=Path)
    make.add_argument("--version", required=True)
    make.add_argument("--target", required=True, choices=TARGETS)
    make.add_argument("--entrypoint", required=True)
    make.add_argument("--protocol", type=int, required=True)
    make.add_argument("--key", type=Path, required=True, help="private Ed25519 key")
    for command in ("verify", "install", "rollback", "launch"):
        action = commands.add_parser(command)
        if command in ("install", "rollback", "launch"):
            action.add_argument("root", type=Path)
        if command in ("verify", "install"):
            action.add_argument("bundle", type=Path)
        action.add_argument("--key", type=Path, required=True, help="trusted Ed25519 public key")
    args = parser.parse_args()
    try:
        if args.command == "package":
            package(args)
        elif args.command == "verify":
            with zipfile.ZipFile(args.bundle) as archive:
                _, _, manifest = signed_archive(archive, args.key)
                read_bundle(archive, manifest)
                print(f'{manifest["version"]} {manifest["target"]} protocol={manifest["protocol"]}')
        else:
            return activate(args)
    except subprocess.CalledProcessError:
        parser.exit(1, "OpenSSL could not sign or authenticate the release. Check the key and signature.\n")
    except (OSError, KeyError, TypeError, ValueError, sqlite3.Error, zipfile.BadZipFile) as error:
        parser.exit(1, f"{error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
