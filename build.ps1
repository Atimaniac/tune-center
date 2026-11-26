# Buduje Tune Center i składa gotowy program w katalogu dist\
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

New-Item -ItemType Directory -Force -Path dist | Out-Null

Write-Host "== Krok 1/2: Rust (cargo build) =="
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build nie powiodl sie (kod $LASTEXITCODE). Blad jest powyzej." }

# Pozostalosci dawnego procesu czujnikow (.NET, LibreHardwareMonitor) z
# poprzednich budowan - program od 0.42.0 czyta czujniki sam.
$old = @("sensor-bridge.*", "LibreHardwareMonitorLib.dll", "BlackSharp.Core.dll", "DiskInfoToolkit.dll",
    "HidSharp.dll", "Mono.Posix.NETStandard.dll", "MonoPosixHelper.dll", "libMonoPosixHelper.dll",
    "RAMSPDToolkit-NDD.dll", "System.CodeDom.dll", "System.IO.Ports.dll", "System.Management.dll", "de-DE", "es-ES")
foreach ($o in $old) { Remove-Item "dist\$o" -Recurse -Force -ErrorAction SilentlyContinue }

Copy-Item target\release\tunecenter.exe dist\ -Force
Copy-Item THIRD-PARTY.md dist\ -Force
Copy-Item LICENSE, PRIVACY.md, PRIVACY.pl.md dist\ -Force

# Zakladka "Procesor" i czujniki potrzebuja oficjalnych, podpisanych modulow PawnIO:
# IntelMSR.bin (Intel) oraz RyzenSMU.bin i AMDFamily17.bin (AMD Ryzen);
# SmbusI801.bin i SmbusPIIX4.bin (SMBus chipsetow Intel i AMD) - RAM;
# LpcIO.bin - uklady Super I/O plyty glownej (temperatury, wentylatory).
# Pobieramy je z najnowszego wydania projektu PawnIO.Modules i kopiujemy bez
# zmian. Numer wydania trafia do dist\modules\version.txt - przy nowszym
# wydaniu wszystkie moduly sa pobierane ponownie.
# Bez nich program dziala, tylko zakladka "Procesor" jest nieaktywna.
Write-Host "== Krok 2/2: moduly PawnIO =="
$modules = @("IntelMSR.bin", "RyzenSMU.bin", "AMDFamily17.bin", "SmbusI801.bin", "SmbusPIIX4.bin", "LpcIO.bin")
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $release = Invoke-RestMethod "https://api.github.com/repos/namazso/PawnIO.Modules/releases/latest" -Headers @{ "User-Agent" = "TuneCenter-build" }
    $have = if (Test-Path "dist\modules\version.txt") { (Get-Content "dist\modules\version.txt" -Raw).Trim() } else { "" }
    $missing = $modules | Where-Object { -not (Test-Path "dist\modules\$_") }
    if ($have -eq $release.tag_name -and -not $missing) {
        Write-Host "Moduly z wydania $have juz sa w dist\modules."
    } else {
        $asset = $release.assets | Where-Object { $_.name -like "*.zip" } | Select-Object -First 1
        if (-not $asset) { throw "W wydaniu $($release.tag_name) nie ma archiwum .zip" }

        $tmp = Join-Path $env:TEMP "tunecenter-pawnio-modules"
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Force -Path $tmp | Out-Null
        Invoke-WebRequest $asset.browser_download_url -OutFile "$tmp\modules.zip" -UseBasicParsing
        Expand-Archive "$tmp\modules.zip" -DestinationPath "$tmp\unpacked" -Force

        New-Item -ItemType Directory -Force -Path "dist\modules" | Out-Null
        foreach ($name in $modules) {
            $bin = Get-ChildItem "$tmp\unpacked" -Recurse -Filter $name | Select-Object -First 1
            if (-not $bin) { throw "W archiwum nie ma pliku $name" }
            Copy-Item $bin.FullName "dist\modules\$name" -Force
        }
        Set-Content "dist\modules\version.txt" $release.tag_name
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Pobrano moduly z wydania $($release.tag_name): $($modules -join ', ')."
    }
} catch {
    if ($modules | Where-Object { -not (Test-Path "dist\modules\$_") }) {
        Write-Warning "Nie udalo sie pobrac modulow PawnIO: $($_.Exception.Message)"
        Write-Warning "Zakladka 'Procesor' bedzie nieaktywna. Pobierz recznie archiwum z"
        Write-Warning "https://github.com/namazso/PawnIO.Modules/releases i skopiuj pliki"
        Write-Warning "$($modules -join ', ') do dist\modules\"
    } else {
        Write-Warning "Nie sprawdzono nowszych modulow PawnIO ($($_.Exception.Message)); zostaja obecne."
    }
}

Write-Host ""
Write-Host "Gotowe. Uruchom dist\tunecenter.exe (CPU, czujniki procesora i LED wymagaja administratora)."
