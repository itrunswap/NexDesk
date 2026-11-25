"""Package the native device directory and the matching browser build."""

import argparse
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

from notices import ROOT


def platform(binary):
    with binary.open("rb") as file:
        header = file.read(64)
        if len(header) == 64 and header[:2] == b"MZ":
            file.seek(struct.unpack_from("<I", header, 60)[0])
            if file.read(6) == b"PE\0\0\x64\x86":
                return "Windows", "x86_64-pc-windows-msvc", "nexdesk-signaling.exe", ".zip"
        if header[:6] == b"\x7fELF\x02\x01" and header[18:20] == b"\x3e\x00":
            return "Linux", "x86_64-unknown-linux-gnu", "bin/nexdesk-signaling", ".tar.gz"
    raise ValueError("expected a Windows x64 or Linux x64 directory executable")


def package(binary):
    os_name, target, entry, suffix = platform(binary)
    manifest = (ROOT / "apps/desktop/pubspec.yaml").read_text(encoding="utf-8")
    version = re.search(r"^version: (\d+\.\d+\.\d+)(?:\+\d+)?$", manifest, re.MULTILINE)
    if not version:
        raise ValueError("expected a release version in apps/desktop/pubspec.yaml")

    web = ROOT / "apps/web/dist"
    if not (web / "index.html").is_file() or not (web / "core/nexdesk_core_bg.wasm").is_file():
        raise ValueError("build the browser with tools/m4/build-web before packaging")

    deploy = ROOT / "deploy"
    files = {
        entry: binary,
        "DEPLOYMENT.md": ROOT / "docs/deployment.md",
        "nginx/nexdesk.conf.example": deploy / "nginx/nexdesk.conf.example",
        "nginx/nexdesk-signaling.service": deploy / "nginx/nexdesk-signaling.service",
        "turn/turnserver.conf": deploy / "turnserver.conf",
        "turn/turn-entrypoint.sh": deploy / "turn-entrypoint.sh",
    }
    for path in sorted(web.rglob("*")):
        if path.is_file():
            files[f"web/{path.relative_to(web).as_posix()}"] = path

    output = ROOT / f"target/release-{version[1]}/NexDesk-{version[1]}-SelfHost-{os_name}-x64{suffix}"
    if output.exists():
        raise FileExistsError(output)
    output.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        notice = Path(temporary) / "THIRD-PARTY-NOTICES.txt"
        subprocess.run(
            [sys.executable, str(ROOT / "tools/release/notices.py"), str(notice),
             "--rust-target", target, "--web"],
            cwd=ROOT,
            check=True,
        )
        files[notice.name] = notice
        pending = tempfile.NamedTemporaryFile(dir=output.parent, prefix=".nexdesk-", delete=False)
        archive = Path(pending.name)
        try:
            with pending:
                if suffix == ".zip":
                    with zipfile.ZipFile(pending, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                        for name, path in files.items():
                            bundle.write(path, name)
                else:
                    with tarfile.open(fileobj=pending, mode="w:gz") as bundle:
                        for name, path in files.items():
                            info = bundle.gettarinfo(str(path), name)
                            info.uid = info.gid = 0
                            info.uname = info.gname = "root"
                            info.mode = 0o755 if name == entry or name.endswith(".sh") else 0o644
                            with path.open("rb") as source:
                                bundle.addfile(info, source)
            archive.rename(output)
        finally:
            archive.unlink(missing_ok=True)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="built nexdesk-signaling executable")
    args = parser.parse_args()
    try:
        print(package(args.binary.resolve()))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
