#!/usr/bin/env bash
# Prepares and builds the Windows app (run in Git Bash on Windows, as CI does):
#   1. Flutter's Windows scaffolding around the committed sources,
#   2. the Rust <-> Dart bindings,
#   3. the Rust core as myterm_ffi.dll,
#   4. flutter build windows, then copies the DLL and the MSVC runtime next to
#      MyTerm.exe so the folder runs on a clean PC.
# Output: apps/mobile/build/windows/x64/runner/Release (zip it to ship).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
APP="$ROOT/apps/mobile"

echo "==> Flutter Windows scaffolding"
cd "$APP"
flutter create --platforms=windows --org dev.myterm --project-name myterm . >/dev/null
rm -f test/widget_test.dart
# Window title and exe name.
sed -i 's/window.Create(L"myterm"/window.Create(L"MyTerm"/' windows/runner/main.cpp
sed -i 's/set(BINARY_NAME "myterm")/set(BINARY_NAME "MyTerm")/' windows/CMakeLists.txt

echo "==> Dart packages and Rust bindings"
flutter pub get
cd "$ROOT"
flutter_rust_bridge_codegen generate
cd "$APP"
dart run build_runner build --delete-conflicting-outputs

echo "==> Rust core (myterm_ffi.dll)"
cd "$ROOT"
cargo build --release -p myterm-ffi

echo "==> Flutter build"
cd "$APP"
flutter build windows --release
OUT="$APP/build/windows/x64/runner/Release"
cp "$ROOT/target/release/myterm_ffi.dll" "$OUT/"
# The Visual C++ runtime Flutter and Rust link against (redistributable).
for dll in msvcp140.dll vcruntime140.dll vcruntime140_1.dll; do
  [ -f "/c/Windows/System32/$dll" ] && cp "/c/Windows/System32/$dll" "$OUT/"
done
ls -la "$OUT"
