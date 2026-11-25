"""Collect dependency notices from the resolved NexDesk build inputs."""

import argparse
import json
import re
import subprocess
from pathlib import Path
from urllib.parse import urljoin, urlsplit
from urllib.request import url2pathname


ROOT = Path(__file__).resolve().parents[2]
NOTICE = re.compile(r"^(licen[cs]e|copying|notice|authors|copyright)([-_.].*)?$", re.IGNORECASE)


def license_files(directory, declared=None):
    paths = {path for path in directory.iterdir() if path.is_file() and NOTICE.match(path.name)}
    if declared:
        path = (directory / declared).resolve()
        if path.is_relative_to(directory) and path.is_file():
            paths.add(path)
    return sorted(paths)


def cargo(target):
    output = subprocess.check_output(
        ("cargo", "metadata", "--format-version=1", "--offline", "--locked", "--filter-platform", target),
        cwd=ROOT,
    )
    for package in json.loads(output)["packages"]:
        if package["source"] is not None:
            directory = Path(package["manifest_path"]).parent.resolve()
            yield (
                f'Rust {package["name"]} {package["version"]}',
                package["license"] or "",
                license_files(directory, package["license_file"]),
            )


def flutter():
    config = ROOT / "apps/desktop/.dart_tool/package_config.json"
    for package in json.loads(config.read_text(encoding="utf-8"))["packages"]:
        if package["name"] == "nexdesk_desktop":
            continue
        resolved = urlsplit(urljoin(config.as_uri(), package["rootUri"]))
        if resolved.scheme != "file" or resolved.netloc:
            raise ValueError("expected a local Dart package")
        directory = Path(url2pathname(resolved.path))
        spec = (directory / "pubspec.yaml").read_text(encoding="utf-8")
        version = re.search(r"^version:\s*([^#\s]+)", spec, re.MULTILINE)
        label = f'Dart {package["name"]} {version[1] if version else "(SDK)"}'
        yield label, "", license_files(directory)


def npm():
    root = ROOT / "apps/web"
    lock = json.loads((root / "package-lock.json").read_text(encoding="utf-8"))
    for path, package in lock["packages"].items():
        if not path or package.get("dev"):
            continue
        directory = root / path
        spec = json.loads((directory / "package.json").read_text(encoding="utf-8"))
        yield f'NPM {spec["name"]} {package["version"]}', spec.get("license", ""), license_files(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--rust-target")
    parser.add_argument("--flutter", action="store_true")
    parser.add_argument("--web", action="store_true")
    parser.add_argument("--extra", action="append", nargs=2, default=[], metavar=("COMPONENT", "FILE"))
    args = parser.parse_args()
    if not (args.rust_target or args.flutter or args.web or args.extra):
        parser.error("choose at least one input to collect")
    try:
        notices = []
        if args.rust_target:
            notices.extend(cargo(args.rust_target))
        if args.flutter:
            notices.extend(flutter())
        if args.web:
            notices.extend(npm())
        for label, path in args.extra:
            notices.append((label, "", [Path(path)]))
        with args.output.open("x", encoding="utf-8", newline="\n") as output:
            output.write("NexDesk third-party notices\n")
            output.write("Resolved dependency inventory and license texts included in the build inputs.\n")
            output.write("Rust and Dart inventories may also contain build/test components.\n")
            for label, license_id, paths in sorted(notices):
                output.write(f"\n{'=' * 72}\n{label}\n")
                if license_id:
                    output.write(f"Declared license: {license_id}\n")
                if not paths:
                    output.write("No license text located in the resolved package root.\n")
                for path in paths:
                    output.write(f"\n{path.name}\n\n")
                    output.write(path.read_text(encoding="utf-8-sig", errors="replace"))
                    output.write("\n")
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
