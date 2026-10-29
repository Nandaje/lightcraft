#!/usr/bin/env bash
# Build LightCraft for Android (arm64-v8a) and assemble the release APK.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
TARGET="aarch64-linux-android"
API_LEVEL="${ANDROID_API_LEVEL:-26}"

if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
    echo "error: ANDROID_NDK_HOME is not set" >&2
    exit 1
fi

HOST_TAG="linux-x86_64"
case "$(uname -s)" in
    Darwin*) HOST_TAG="darwin-x86_64" ;;
    MINGW*|MSYS*|CYGWIN*) HOST_TAG="windows-x86_64" ;;
esac

TOOLCHAIN_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$TOOLCHAIN_BIN/${TARGET}${API_LEVEL}-clang"
export CC_aarch64_linux_android="$TOOLCHAIN_BIN/${TARGET}${API_LEVEL}-clang"
export AR_aarch64_linux_android="$TOOLCHAIN_BIN/llvm-ar"

echo "==> Building liblightcraft_android.so ($TARGET, release)"
cargo build --release --target "$TARGET" -p lightcraft-android

JNI_DIR="$ROOT_DIR/packaging/android/app/src/main/jniLibs/arm64-v8a"
mkdir -p "$JNI_DIR"
cp "target/$TARGET/release/liblightcraft_android.so" "$JNI_DIR/liblightcraft_android.so"

RES_DIR="$ROOT_DIR/packaging/android/app/src/main/res"
mkdir -p "$RES_DIR/mipmap-mdpi" "$RES_DIR/mipmap-hdpi" "$RES_DIR/mipmap-xhdpi" "$RES_DIR/mipmap-xxhdpi" "$RES_DIR/mipmap-xxxhdpi"
cp "$ROOT_DIR/assets/app-icon/hicolor/48x48/apps/ai.storyteller.lightcraft.png" "$RES_DIR/mipmap-mdpi/ic_launcher.png"
cp "$ROOT_DIR/assets/app-icon/hicolor/64x64/apps/ai.storyteller.lightcraft.png" "$RES_DIR/mipmap-hdpi/ic_launcher.png"
cp "$ROOT_DIR/assets/app-icon/hicolor/128x128/apps/ai.storyteller.lightcraft.png" "$RES_DIR/mipmap-xhdpi/ic_launcher.png"
cp "$ROOT_DIR/assets/app-icon/hicolor/256x256/apps/ai.storyteller.lightcraft.png" "$RES_DIR/mipmap-xxhdpi/ic_launcher.png"
cp "$ROOT_DIR/assets/app-icon/hicolor/512x512/apps/ai.storyteller.lightcraft.png" "$RES_DIR/mipmap-xxxhdpi/ic_launcher.png"

echo "==> Assembling Android release APK via Gradle"
cd "$ROOT_DIR/packaging/android"
gradle assembleRelease --no-daemon

OUT_DIR="$ROOT_DIR/dist/release"
mkdir -p "$OUT_DIR"
APK_PATH="$OUT_DIR/lightcraft-${VERSION}-android-arm64-v8a.apk"
cp "app/build/outputs/apk/release/app-release.apk" "$APK_PATH"
echo "==> Built APK: $APK_PATH"
