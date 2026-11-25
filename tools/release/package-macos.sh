#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
    printf '%s\n' 'Build the macOS package on a Mac with Xcode installed.' >&2
    exit 2
fi

case $(uname -m) in
    arm64) architecture=arm64; rust_target=aarch64-apple-darwin; package_arch=arm64 ;;
    x86_64) architecture=x86_64; rust_target=x86_64-apple-darwin; package_arch=x64 ;;
    *) printf '%s\n' 'Unsupported macOS architecture.' >&2; exit 2 ;;
esac

root=$(cd "${NEXDESK_SOURCE_ROOT:-$(dirname "${BASH_SOURCE[0]}")/../..}" && pwd)
cd "$root"
version=$(awk '$1 == "version:" { split($2, parts, "+"); print parts[1] }' apps/desktop/pubspec.yaml)
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf '%s\n' 'Expected a release version in apps/desktop/pubspec.yaml.' >&2
    exit 2
fi

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/target}
export MACOSX_DEPLOYMENT_TARGET=12.0
mkdir -p "$CARGO_TARGET_DIR" "target/release-$version"
export CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
output="$root/target/release-$version/NexDesk-$version-Desktop-macOS-$package_arch.zip"
if [[ -e $output ]]; then
    printf 'Archive already exists: %s\n' "$output" >&2
    exit 2
fi

cargo fetch --locked --target "$rust_target"
cargo build --release --locked --target "$rust_target" -p nexdesk-agent -p nexdesk-ffi -p nexdesk-cli
native="$CARGO_TARGET_DIR/$rust_target/release"

cd "$root/apps/desktop"
flutter pub get --enforce-lockfile
if [[ -n ${NEXDESK_DEFAULT_DIRECTORY:-} ]]; then
    flutter build macos --release --config-only "--dart-define=NEXDESK_DEFAULT_DIRECTORY=$NEXDESK_DEFAULT_DIRECTORY"
else
    flutter build macos --release --config-only
fi
xcodebuild -workspace macos/Runner.xcworkspace -scheme Runner -configuration Release \
    -destination 'generic/platform=macOS' -derivedDataPath "$PWD/build/macos" \
    "ARCHS=$architecture" ONLY_ACTIVE_ARCH=NO CODE_SIGNING_ALLOWED=NO

app="$PWD/build/macos/Build/Products/Release/NexDesk.app"
for name in nexdesk-agent nexdesk-cli libnexdesk_ffi.dylib; do
    install -m 755 "$native/$name" "$app/Contents/MacOS/$name"
done
for name in NexDesk nexdesk-agent nexdesk-cli libnexdesk_ffi.dylib; do
    lipo "$app/Contents/MacOS/$name" -verify_arch "$architecture"
done
"$app/Contents/MacOS/nexdesk-cli" check-id 012345678

notices=$(mktemp -d "${TMPDIR:-/tmp}/nexdesk-notices.XXXXXX")
trap 'rm -f "$notices/THIRD-PARTY-NOTICES.txt"; rmdir "$notices"' EXIT
python3 "$root/tools/release/notices.py" "$notices/THIRD-PARTY-NOTICES.txt" \
    --rust-target "$rust_target" --flutter
install -m 644 "$notices/THIRD-PARTY-NOTICES.txt" "$app/Contents/Resources/THIRD-PARTY-NOTICES.txt"

if [[ -n ${NEXDESK_MACOS_SIGN_IDENTITY:-} ]]; then
    bash "$root/tools/release/sign-macos.sh" "$app" "$output"
else
    for component in "$app"/Contents/Frameworks/*.framework "$app"/Contents/Frameworks/*.dylib; do
        if [[ -e $component ]]; then codesign --force --sign - "$component"; fi
    done
    for name in nexdesk-agent nexdesk-cli libnexdesk_ffi.dylib; do
        codesign --force --sign - "$app/Contents/MacOS/$name"
    done
    codesign --force --sign - "$app"
    codesign --verify --deep --strict "$app"
    ditto -c -k --keepParent "$app" "$output"
fi

cd "$(dirname "$output")"
archive=$(basename "$output")
shasum -a 256 "$archive" > "$archive.sha256"
printf 'Packaged %s\n' "$output"
