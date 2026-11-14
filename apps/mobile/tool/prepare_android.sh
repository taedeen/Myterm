#!/usr/bin/env bash
# Prepares the Android build:
#   1. generates Flutter's Android scaffolding around the committed sources
#      (flutter create never overwrites existing files such as our
#      MainActivity.kt and AndroidManifest.xml)
#   2. applies Gradle settings MyTerm needs (minSdk 30, androidx.biometric)
#   3. generates the Rust <-> Dart bindings (flutter_rust_bridge)
#   4. cross-compiles the Rust core into jniLibs with cargo-ndk
#
# Needs: flutter, rustup targets for Android, cargo-ndk,
# flutter_rust_bridge_codegen 2.11.1, ANDROID_NDK_HOME.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
APP="$ROOT/apps/mobile"
TARGETS="${MYTERM_ANDROID_TARGETS:-arm64-v8a armeabi-v7a x86_64}"
PROFILE="${MYTERM_RUST_PROFILE:-release}"

echo "==> Flutter Android scaffolding"
cd "$APP"
flutter create --platforms=android --org dev.myterm --project-name myterm . >/dev/null
rm -f test/widget_test.dart   # template test refers to the counter app

echo "==> Gradle settings"
GRADLE="android/app/build.gradle.kts"
[ -f "$GRADLE" ] || GRADLE="android/app/build.gradle"
# minSdk 30: per-use Keystore authentication with BIOMETRIC_STRONG | DEVICE_CREDENTIAL.
sed -i.bak -E \
  -e 's/minSdk = flutter\.minSdkVersion/minSdk = 30/' \
  -e 's/minSdkVersion flutter\.minSdkVersion/minSdkVersion 30/' "$GRADLE"
rm -f "$GRADLE.bak"
grep -Eq 'minSdk(Version)? (= )?30' "$GRADLE" || { echo "could not set minSdk 30 in $GRADLE" >&2; exit 1; }
if ! grep -q "androidx.biometric" "$GRADLE"; then
  if [[ "$GRADLE" == *.kts ]]; then
    printf '\ndependencies {\n    implementation("androidx.biometric:biometric:1.1.0")\n    implementation("androidx.core:core-ktx:1.13.1")\n}\n' >> "$GRADLE"
  else
    printf "\ndependencies {\n    implementation 'androidx.biometric:biometric:1.1.0'\n    implementation 'androidx.core:core-ktx:1.13.1'\n}\n" >> "$GRADLE"
  fi
fi

echo "==> Dart packages and Rust bindings"
flutter pub get
cd "$ROOT"
flutter_rust_bridge_codegen generate
cd "$APP"
dart run build_runner build --delete-conflicting-outputs

echo "==> Rust core for Android ($TARGETS, $PROFILE)"
cd "$ROOT"
NDK_ARGS=()
for t in $TARGETS; do NDK_ARGS+=(-t "$t"); done
PROFILE_ARG=()
[ "$PROFILE" = "release" ] && PROFILE_ARG=(--release)
cargo ndk "${NDK_ARGS[@]}" -o "$APP/android/app/src/main/jniLibs" build "${PROFILE_ARG[@]}" -p myterm-ffi

echo "==> Ready: cd apps/mobile && flutter run   (or flutter build apk --release)"
