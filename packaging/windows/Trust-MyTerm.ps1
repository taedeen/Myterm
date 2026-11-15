<#
.SYNOPSIS
  Trusts MyTerm's code-signing certificate on this PC, so the installer and
  app show "Verified publisher: MyTerm". Run once per PC, in an administrator
  PowerShell, from the folder holding the release files:

    powershell -ExecutionPolicy Bypass -File .\Trust-MyTerm.ps1

  Uses myterm-root.cer and myterm-codesign.cer from the same folder. The root
  can only vouch for code signing, and its private key no longer exists.
  Also unblocks MyTerm installers in this folder, so Windows doesn't treat
  them as unknown downloads.
#>
[CmdletBinding()]
param([string]$Dir = $PSScriptRoot)
$ErrorActionPreference = 'Stop'

$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this from an administrator PowerShell.'
}
$root = Join-Path $Dir 'myterm-root.cer'
$leaf = Join-Path $Dir 'myterm-codesign.cer'
foreach ($f in $root, $leaf) { if (-not (Test-Path $f)) { throw "Missing $f (download it from the release)." } }

$r = Import-Certificate -FilePath $root -CertStoreLocation Cert:\LocalMachine\Root
$l = Import-Certificate -FilePath $leaf -CertStoreLocation Cert:\LocalMachine\TrustedPublisher
Write-Host "Trusted: $($r.Subject)  [$($r.Thumbprint)]"
Write-Host "Trusted publisher: $($l.Subject)  [$($l.Thumbprint)]"

Get-ChildItem -Path $Dir -Filter 'MyTerm-*-setup.exe' -ErrorAction SilentlyContinue | ForEach-Object {
    Unblock-File $_.FullName
    $sig = Get-AuthenticodeSignature $_.FullName
    Write-Host "$($_.Name): signature $($sig.Status)"
}
Write-Host 'Done.' -ForegroundColor Green
