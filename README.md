# MyTerm

A multi-platform SSH management tool: connect to many hosts, with autocomplete, an encrypted credential vault, shortcut keys (including a secure **Type password** key), end-to-end encrypted sync, and hardware-bound keys.

Build order: **foundation → Android → Windows**, then Linux and iOS.

**Using MyTerm:** see the [user guide](docs/user-guide.md): deploying the sync server, installing the apps, and everyday use.

## Repository layout

```
crates/
  core/    myterm-core   Std-only foundation, no dependencies. Unit-tested.
  vault/   myterm-vault  Encrypted vault: Argon2id + XChaCha20-Poly1305 + HKDF.
  ssh/     myterm-ssh    SSH sessions on russh: jump hosts, host-key checks,
                         password / key / hardware-key auth, PTY shell.
  ffi/     myterm-ffi    API for the Flutter apps (flutter_rust_bridge).
apps/
  mobile/  Flutter app (Android first; iOS later from the same code).
           android/app/src/main/kotlin: Keystore/StrongBox keys,
           biometric prompts, device-key unwrap, keep-alive service.
.github/workflows/ci.yml   Tests, clippy and the Android APK build.
```

## What's in the foundation (`myterm-core`)

| Module | Spec | What it does |
|---|---|---|
| `model` | 3.1 | Hosts, groups, nearest-wins inheritance, jump-chain checks |
| `keybar` | 3.5 A | Key-bar profiles; `ctrl+B, %` style sequences to bytes; tmux/vim/basic profiles |
| `prompt` | 3.3, 3.5 C | Detects password, sudo, passphrase and OTP prompts in terminal output |
| `inject` | 3.5 C | Type password decisions (override, biometric confirm, never-auto-type) and a self-wiping `SecretBuf` |
| `scrub` | 3.3 | Drops commands that contain secrets before they reach history |
| `history` | 8.2 | Per-device command history; conflict-free merge (G-counters, tombstones) |
| `completion` | 3.3 | Ranked suggestions: snippets, host history, related-host history, command specs, remote hints |
| `session` | 3.3, 3.5 C | Tracks typed lines and on-screen prompts; performs Type password on a channel |
| `sshkey` | 3.4.1 | SSH key blobs, authorized_keys lines, SHA256 fingerprints, DER ECDSA signature to SSH wire format |
| `signer` | 3.4.1 | Hardware signer interface, protection levels, "hardware keys only" policy |
| `knownhosts` | 3.4 | Trust on first use; a changed key is a hard stop; OpenSSH known_hosts import |
| `sync` | 8.3 | Per-field last-writer-wins records with version vectors; device-only records never upload |
| `audit` | 6.3 | Hash-chained audit log with tamper detection and JSONL export |
| `codec` | — | Binary encoding for the store and sync payloads |

## Building

### Core tests (any machine with Rust 1.88+)

```sh
cargo test -p myterm-core
cargo clippy -p myterm-core --all-targets -- -D warnings
```

### Android app

Requirements: Flutter (stable), Java 17, Android SDK + NDK, Rust with Android targets, `cargo-ndk`, `flutter_rust_bridge_codegen` 2.11.1.

```sh
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
cargo install flutter_rust_bridge_codegen --version 2.11.1 --locked
export ANDROID_NDK_HOME=/path/to/ndk

apps/mobile/tool/prepare_android.sh   # scaffolding, bindings, Rust .so files
cd apps/mobile
flutter run                            # or: flutter build apk --release
```

Every push also builds the APK in GitHub Actions. Download it from the run's **Artifacts** section (`myterm-android-apk`).

The app needs Android 11 (API 30) or later, for per-use Keystore authentication.

## Security notes

- Hosts, credentials, snippets and history are vault records. Everything is encrypted on the device, and the sync payload is ciphertext.
- Secrets never cross into Dart after they're stored. Type password writes them from Rust straight to the SSH channel.
- Hardware keys are generated in StrongBox where available, otherwise the TEE. Every signature needs biometrics or the device PIN. Keys are invalidated when biometrics change, and they never sync.
- The vault master key is wrapped twice: with Argon2id from the master password, and with a Keystore AES key that needs biometrics on every use.

## Status

Version 0.4.0 (see [CHANGELOG.md](CHANGELOG.md)).

| Part | State |
|---|---|
| `myterm-core`, `myterm-vault`, `myterm-ssh`, `myterm-ffi` | Built and tested in CI (tests + clippy `-D warnings`) |
| Android app | Released: APK signed with the MyTerm key, installs over earlier builds |
| Windows app | Released: signed installer + portable zip ([packaging/windows](packaging/windows/README.md)) |
| Sync server | Released: Docker image, device list and revocation ([server](server/README.md)) |
| Linux command line | Released: `myterm` for x86_64 and aarch64 ([crates/cli](crates/cli/README.md)) |
| Server tools | `deploy/`: upgrade.sh (Docker apps and release binaries), bootstrap.sh, deploy-apk.sh |
| Not yet | iOS, OpenSSH agent integration for the command line, TPM-backed SSH keys on Windows, FIDO2 keys |

## Releasing

Bump `version:` in `apps/mobile/pubspec.yaml` and `lib/src/version.dart`, add
a section to `CHANGELOG.md`, and merge to `main`. When `main` has a version
with no release yet, CI builds everything, checks the Windows signatures, tags
`vX.Y.Z` and publishes the GitHub release with the APK, installer, portable
zip and certificates.
