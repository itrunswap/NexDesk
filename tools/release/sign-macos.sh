#!/bin/sh
set -eu

if [ "$#" -ne 2 ] || [ ! -d "$1/Contents/MacOS" ]; then
    printf '%s\n' "Usage: sign-macos.sh APP_PATH OUTPUT_ZIP" >&2
    exit 2
fi

: "${NEXDESK_MACOS_SIGN_IDENTITY:?set the Developer ID Application identity}"
app=$1
output=$2
[ ! -e "$output" ] || { printf '%s\n' "Output already exists." >&2; exit 2; }

sign() {
    codesign --force --options runtime --timestamp --sign "$NEXDESK_MACOS_SIGN_IDENTITY" "$@"
}

for component in "$app"/Contents/Frameworks/*.framework "$app"/Contents/Frameworks/*.dylib; do
    if [ -e "$component" ]; then sign "$component"; fi
done
sign "$app/Contents/MacOS/nexdesk-agent"
sign "$app/Contents/MacOS/libnexdesk_ffi.dylib"
if [ -f "$app/Contents/MacOS/nexdesk-cli" ]; then
    sign "$app/Contents/MacOS/nexdesk-cli"
fi
sign "$app"
codesign --verify --deep --strict "$app"

if [ -n "${NEXDESK_NOTARY_PROFILE:-}" ]; then
    archive=$(mktemp -d "${TMPDIR:-/tmp}/nexdesk-notary.XXXXXX")
    trap 'rm -f "$archive/submit.zip"; rmdir "$archive"' EXIT
    ditto -c -k --keepParent "$app" "$archive/submit.zip"
    xcrun notarytool submit "$archive/submit.zip" --keychain-profile "$NEXDESK_NOTARY_PROFILE" --wait
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    spctl --assess --type execute "$app"
fi

ditto -c -k --keepParent "$app" "$output"
