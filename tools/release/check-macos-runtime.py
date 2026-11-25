"""Check the installed bundle's FFI and managed receiver without capturing a screen."""

import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from uuid import uuid4


def receiver(agent, env, *, volatile, codec):
    command = [str(agent), "host", "--managed", "--codec", codec]
    if volatile:
        command.append("--volatile")
    label = f"{codec} {'temporary' if volatile else 'Keychain-backed'} startup"
    try:
        run = subprocess.run(
            command,
            input='{"command":"stop"}\n',
            capture_output=True,
            text=True,
            env=env,
            timeout=18,
            check=False,
        )
    except subprocess.TimeoutExpired:
        print(f"{label}: timed out before the receiver finished", flush=True)
        return None
    for line in run.stdout.splitlines():
        if line.startswith("{"):
            event = json.loads(line)
            if event.get("event") == "ready" and event.get("managed") is True and run.returncode == 0:
                print(f"{label}: ready", flush=True)
                return event["identity"], event["directory_identity"]
    if run.returncode != 0:
        print(f"{label}: receiver exited with status {run.returncode}", flush=True)
        for line in run.stderr.splitlines():
            if line.startswith("NexDesk agent:") or "dyld" in line:
                print(line, flush=True)
    else:
        print(f"{label}: receiver exited before reporting ready", flush=True)
    return None


def check(app):
    binaries = app / "Contents/MacOS"
    library = ctypes.CDLL(str(binaries / "libnexdesk_ffi.dylib"))
    library.nexdesk_abi_version.restype = ctypes.c_uint32
    if library.nexdesk_abi_version() != 1:
        raise ValueError("unexpected NexDesk native ABI")
    print("Native library ABI: ready", flush=True)
    agent = binaries / "nexdesk-agent"
    with tempfile.TemporaryDirectory(prefix="nexdesk-macos-check-") as profile:
        env = {
            **os.environ,
            "NEXDESK_DATA_DIR": profile,
            "NEXDESK_CREDENTIAL_NAMESPACE": f"runtime-{uuid4().hex}",
        }
        if not receiver(agent, env, volatile=True, codec="h264"):
            receiver(agent, env, volatile=True, codec="vp8")
            raise ValueError("default receiver media initialization failed")
        first = receiver(agent, env, volatile=False, codec="h264")
        if first is None:
            raise ValueError("the managed receiver could not access its startup credentials")
        second = receiver(agent, env, volatile=False, codec="h264")
        if first != second:
            raise ValueError("receiver identity could not be restored after restart")
        print("LAN and ID receiver identities survived restart", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="path to NexDesk.app")
    args = parser.parse_args()
    try:
        check(args.app.resolve(strict=True))
    except (OSError, ValueError) as error:
        print(f"NexDesk bundle check: {error}", file=sys.stderr)
        raise SystemExit(1) from None
