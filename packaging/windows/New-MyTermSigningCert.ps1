<#
.SYNOPSIS
  Creates MyTerm's Windows code-signing certificate. Run once, on your own PC,
  in an administrator PowerShell:

    powershell -ExecutionPolicy Bypass -File .\New-MyTermSigningCert.ps1

.DESCRIPTION
  1. Makes a private root certificate that can only vouch for code signing
     (no websites, no further certificate authorities), and a "MyTerm"
     signing certificate issued by it.
  2. Trusts them on this PC (Trusted Root + Trusted Publishers), so MyTerm
     builds signed with it show "Verified publisher: MyTerm".
  3. Deletes the root's private key, so nothing else can ever be issued
     under it. Only the signing key is kept, for GitHub Actions.
  4. Puts the two GitHub secrets on the clipboard in turn
     (WINDOWS_SIGN_PFX, WINDOWS_SIGN_PASSWORD) for you to paste into the
     repository's Actions secrets, then deletes the local copy of the key.

  Your other PCs only need the public certificates: run Trust-MyTerm.ps1
  (shipped with each release) on them.

  -Ci is for the CI self-test: no prompts, writes the files to -OutDir.
#>
[CmdletBinding()]
param(
    [string]$OutDir = (Join-Path ([IO.Path]::GetTempPath()) 'myterm-signing'),
    [string]$Repo = 'taedeen/Myterm',
    [switch]$Ci
)
$ErrorActionPreference = 'Stop'

$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from an administrator PowerShell: it trusts the certificate for this PC.'
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# Root: may only sign one level of certificates (pathlength 0), and only
# for code signing (the extended key usage limits everything under it).
$root = New-SelfSignedCertificate `
    -Subject 'CN=MyTerm Code Signing Root, O=MyTerm' `
    -CertStoreLocation Cert:\CurrentUser\My `
    -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 `
    -KeyUsage CertSign, CRLSign -KeyExportPolicy NonExportable `
    -TextExtension @('2.5.29.19={critical}{text}ca=1&pathlength=0', '2.5.29.37={text}1.3.6.1.5.5.7.3.3') `
    -NotAfter (Get-Date).AddYears(10)

# The signing certificate (what Windows shows as the publisher).
$leaf = New-SelfSignedCertificate -Type CodeSigningCert `
    -Subject 'CN=MyTerm, O=MyTerm' -Signer $root `
    -CertStoreLocation Cert:\CurrentUser\My `
    -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears(5)

$rootCer = Join-Path $OutDir 'myterm-root.cer'
$leafCer = Join-Path $OutDir 'myterm-codesign.cer'
Export-Certificate -Cert $root -FilePath $rootCer | Out-Null
Export-Certificate -Cert $leaf -FilePath $leafCer | Out-Null

# Trust on this PC.
Import-Certificate -FilePath $rootCer -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
Import-Certificate -FilePath $leafCer -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null

# The key for CI, with the root's public certificate included so CI can
# publish both .cer files with each release.
$bytes = New-Object byte[] 24
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
$password = ([Convert]::ToBase64String($bytes)) -replace '[+/=]', ''
$pfx = Join-Path $OutDir 'myterm-codesign.pfx'
Export-PfxCertificate -Cert $leaf -FilePath $pfx -ChainOption BuildChain `
    -Password (ConvertTo-SecureString $password -AsPlainText -Force) | Out-Null

# No private keys left in the certificate store; the root's is gone for good.
Remove-Item -Path "Cert:\CurrentUser\My\$($root.Thumbprint)" -DeleteKey
Remove-Item -Path "Cert:\CurrentUser\My\$($leaf.Thumbprint)" -DeleteKey

if ($Ci) {
    Set-Content -Path (Join-Path $OutDir 'password.txt') -Value $password -NoNewline
    Write-Host "Test certificate written to $OutDir"
    return
}

$url = "https://github.com/$Repo/settings/secrets/actions/new"
Write-Host ''
Write-Host 'Certificate created and trusted on this PC.' -ForegroundColor Green
Write-Host "Now add two repository secrets at:`n  $url" -ForegroundColor Cyan
Start-Process $url
Set-Clipboard -Value ([Convert]::ToBase64String([IO.File]::ReadAllBytes($pfx)))
Read-Host "`n1) Name: WINDOWS_SIGN_PFX   Secret: paste (it's on the clipboard). Click 'Add secret', then press Enter here"
Set-Clipboard -Value $password
Read-Host "`n2) Click 'New repository secret' again. Name: WINDOWS_SIGN_PASSWORD   Secret: paste. Add it, then press Enter here"
Set-Clipboard -Value ' '
Remove-Item $pfx
Write-Host "`nDone. The signing key now exists only in GitHub's encrypted secrets." -ForegroundColor Green
Write-Host "Public certificates (safe to share): $OutDir"
