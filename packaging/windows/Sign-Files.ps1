<#
.SYNOPSIS
  CI: signs files with the MyTerm certificate (SHA-256, RFC 3161 timestamp).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Pfx,
    [Parameter(Mandatory)][string]$Password,
    [Parameter(Mandatory)][string[]]$Files
)
$ErrorActionPreference = 'Stop'
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
    Sort-Object { [version]($_.Directory.Parent.Name) } -Descending | Select-Object -First 1
if (-not $signtool) { throw 'signtool.exe not found (Windows SDK)' }
$timestamps = 'http://timestamp.digicert.com', 'http://timestamp.sectigo.com', 'http://timestamp.globalsign.com/tsa/r6advanced1'
foreach ($f in $Files) {
    $ok = $false
    foreach ($ts in $timestamps) {
        & $signtool.FullName sign /q /fd SHA256 /f $Pfx /p $Password /tr $ts /td SHA256 /d MyTerm $f
        if ($LASTEXITCODE -eq 0) { $ok = $true; break }
        Write-Warning "Timestamping via $ts failed for $f; trying the next server"
        Start-Sleep -Seconds 3
    }
    if (-not $ok) { throw "Signing $f failed" }
    Write-Host "Signed $f"
}
