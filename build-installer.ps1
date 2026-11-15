# Buduje program (build.ps1), a potem instalator: installer-output\TuneCenter-Setup.exe
# Wymaga Inno Setup 6.3+:  winget install JRSoftware.InnoSetup
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$candidates = @(
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
)
$iscc = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    throw "Nie znaleziono Inno Setup 6. Zainstaluj: winget install JRSoftware.InnoSetup"
}

& "$PSScriptRoot\build.ps1"

Write-Host "== Instalator (Inno Setup) =="
& $iscc installer.iss
if ($LASTEXITCODE -ne 0) { throw "Budowanie instalatora nie powiodlo sie (kod $LASTEXITCODE). Blad jest powyzej." }

Write-Host ""
Write-Host "Gotowe: installer-output\TuneCenter-Setup.exe"
