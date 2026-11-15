# Development signing key

`myterm-dev.keystore` signs every CI build of the Android app, so a new APK
installs as an **update** over the previous one. (Without it, each CI runner
signs with a fresh random debug key and Android refuses the update with
"App not installed".)

- Alias `androiddebugkey`, store and key password `android`: the standard
  Android debug-key convention, so CI just copies it to `~/.android/debug.keystore`
  and Flutter's default release signing picks it up. No Gradle changes.
- SHA-256 certificate fingerprint:
  `59:06:1E:3D:FC:8E:9C:2E:6F:8A:D4:52:BA:07:B1:F7:B1:0F:E8:17:F3:05:0A:CD:31:03:82:09:E8:96:63:22`

**For development only.** Anyone who can read this repository can sign an
APK that installs over yours. Before any public release, create a release
key, keep it out of the repo (GitHub Actions secrets), and point the
`android` CI job at it. Changing keys means one more uninstall/reinstall.
