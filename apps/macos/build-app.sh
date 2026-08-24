#!/bin/sh
set -eu

configuration="${1:-debug}"
case "$configuration" in
    debug)
        cargo_configuration=""
        ;;
    release)
        cargo_configuration="--release"
        ;;
    *)
        echo "usage: $0 [debug|release]" >&2
        exit 2
        ;;
esac

package_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_directory=$(CDPATH= cd -- "$package_directory/../.." && pwd)
bundle="$package_directory/.build/HowlerMac.app"

MACOSX_DEPLOYMENT_TARGET=13.0 cargo build \
    --manifest-path "$repository_directory/Cargo.toml" \
    $cargo_configuration \
    -p howler-application-ffi

swift build --package-path "$package_directory" -c "$configuration"
binary_directory=$(swift build --package-path "$package_directory" -c "$configuration" --show-bin-path)

mkdir -p "$bundle/Contents/MacOS"
install -m 755 "$binary_directory/HowlerMac" "$bundle/Contents/MacOS/HowlerMac"
install -m 644 "$package_directory/Resources/Info.plist" "$bundle/Contents/Info.plist"
codesign --force --sign - "$bundle"

printf '%s\n' "$bundle"
