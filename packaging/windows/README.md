# Windows packaging and signing

CI builds `MyTerm-<version>-setup.exe` (Inno Setup, `myterm.iss`) and a
portable zip, and signs the app, its DLLs, the installer and the uninstaller.

## The signing certificate (once)

Windows only trusts code signed by a certificate it knows. Public
certificates cost money and, for individuals in the UK, Microsoft's Azure
Artifact Signing isn't available. MyTerm uses its own certificate instead,
trusted on your PCs:

1. On your main PC, in an **administrator** PowerShell, in this folder:
   `powershell -ExecutionPolicy Bypass -File .\New-MyTermSigningCert.ps1`
   It makes the certificate, trusts it on that PC, and walks you through
   pasting two secrets into GitHub (`WINDOWS_SIGN_PFX`, `WINDOWS_SIGN_PASSWORD`).
2. On each other PC: download `Trust-MyTerm.ps1`, `myterm-root.cer` and
   `myterm-codesign.cer` from a release into one folder and run
   `powershell -ExecutionPolicy Bypass -File .\Trust-MyTerm.ps1` as administrator.

The root certificate can only vouch for code signing (not websites) and only
one level deep, and its private key is deleted once the signing certificate
is made. The signing key lives only in GitHub's encrypted secrets.

Without the secrets, CI signs with a throwaway test certificate (to test the
pipeline); tagged releases refuse to publish those builds.

## What Windows shows

- On a PC that has run the trust script: the installer and app show
  **Verified publisher: MyTerm**, and nothing asks for admin.
- **SmartScreen** ("Windows protected your PC") judges files downloaded in a
  browser by reputation with Microsoft, which a private certificate can't
  earn. `Trust-MyTerm.ps1` unblocks installers in its folder, or right-click
  the downloaded installer → Properties → **Unblock** before running it.
  Downloading with `gh release download` doesn't mark the file at all.
- On PCs that haven't trusted the certificate, Windows treats MyTerm as an
  unknown publisher, as before.
