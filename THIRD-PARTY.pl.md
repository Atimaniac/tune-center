*English: [THIRD-PARTY.md](THIRD-PARTY.md)*

# Składniki innych autorów

Tune Center korzysta z poniższych składników. Są dołączane bez zmian.

## PawnIO.Modules - IntelMSR.bin, RyzenSMU.bin, AMDFamily17.bin, SmbusI801.bin, SmbusPIIX4.bin, LpcIO.bin

- Źródło: https://github.com/namazso/PawnIO.Modules
- Autor: namazso
- Licencja: LGPL-2.1-or-later
- Pliki w `modules\` to oficjalne, podpisane moduły pobrane z wydania
  projektu. Ich kod źródłowy (`*.p`) jest dostępny pod adresem wyżej.

## PawnIO (sterownik i PawnIOLib.dll)

- Źródło: https://github.com/namazso/PawnIO, https://pawnio.eu
- Licencja: GPL-2.0
- Nie jest dołączany; instaluje się go osobno (instalator Tune Center może
  to zrobić przez winget).

## LibreHardwareMonitor - tablice czujników płyt głównych

- Źródło: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- Licencja: MPL-2.0
- Plik `src/hw/boards.rs` zawiera tablice płyt z jego `SuperIOHardware.cs` i
  `Identification.cs` (nazwy temperatur, wentylatorów i sterowania dla
  konkretnych płyt). Ten plik pozostaje na licencji MPL-2.0; sama biblioteka
  nie jest używana ani dołączana.

## Inter (czcionka interfejsu)

- Pliki: `assets/Inter-Regular.ttf`, `assets/Inter-SemiBold.ttf` i
  `assets/Inter-BoldItalic.ttf` (Inter 4.1), wbudowane w `tunecenter.exe`
- Źródło: https://github.com/rsms/inter
- Autor: The Inter Project Authors
- Licencja: SIL Open Font License 1.1

## Odniesienia użyte przy pisaniu kodu

- LibreHardwareMonitor (https://github.com/LibreHardwareMonitor/LibreHardwareMonitor,
  MPL-2.0): układ rejestrów Super I/O (Nuvoton, ITE, Fintek) oraz źródła
  czujników procesora i pamięci; nazwy i identyfikatory czujników są zgodne,
  żeby zapisane ustawienia wentylatorów działały dalej. Kod nie został
  skopiowany.
- RAMSPDToolkit (https://github.com/Blacktempel/RAMSPDToolkit): układ SPD
  czujników temperatury pamięci DDR4/DDR5.
- PowerMonkey (https://github.com/psyq321/PowerMonkey, Apache-2.0): układ pól
  i numery poleceń skrzynki OC procesorów Intel (MSR 0x150).
- ZenStates-Core (https://github.com/irusanov/ZenStates-Core, GPL-3.0): numery
  poleceń SMU procesorów Ryzen, kodowanie maski rdzenia i adresy bezpieczników
  z mapą rdzeni. Użyte jako odniesienie; kod nie został skopiowany.
- ryzen_monitor (https://github.com/hattedsquirrel/ryzen_monitor) i rigstats
  (https://github.com/dvalfrid/rigstats): położenie limitów PPT, TDC i EDC
  w tabeli PM procesorów Ryzen 5000 i 9000.
- OpenRGB (https://gitlab.com/CalcProgrammer1/OpenRGB, GPL-2.0-or-later):
  fakty o protokołach podświetlenia - taśmy Skydimo (zapytanie "Moni-A",
  tabela modeli, ramka Adalight), rejestry ENE pamięci RAM, funkcje RGB
  Logitech HID++, układ pakietów i numery poleceń ASUS Aura USB, Gigabyte RGB
  Fusion 2, Corsair Lighting Node i MSI Mystic Light. Użyte jako odniesienie;
  kod nie został skopiowany.
- NVIDIA NVAPI SDK (https://github.com/NVIDIA/nvapi, MIT): identyfikatory
  funkcji, struktury i wartości ustawień sterownika (`nvapi.h`,
  `nvapi_interface.h`, `NvApiDriverSettings.h`) oraz przykłady
  DisplayConfiguration i DisplayColorControl - podstawa
  zakładki NVIDIA. Kod SDK nie jest dołączany; program woła sterownik przez
  `nvapi64.dll`.
- AMD ADLX SDK (https://github.com/GPUOpen-LibrariesAndSDKs/ADLX): układ
  interfejsów C (`SDK/Include`) dla zakładki RADEON oraz zegarów, wentylatorów i czujników kart Radeon. Program woła sterownik
  przez `amdadlx64.dll`.
- Intel Graphics Control Library (https://github.com/intel/drivers.gpu.control-library,
  licencja Intel Software License Agreement, Copyright (C) Intel Corporation):
  struktury i funkcje z `include/igcl_api.h` dla zakładki INTEL. Nagłówek ani
  biblioteka nie są dołączane; program woła bibliotekę sterownika Intela
  `ControlLib.dll`, wyłącznie na platformach Intela.
- CUE4Parse (https://github.com/FabianFG/CUE4Parse, Apache-2.0): układ paczek
  Unreal Engine `.pak` / IoStore i archiwów shaderów (`.ushaderbytecode`)
  oraz pamięci potoków DirectX 12 (`.upipelinecache`) i spakowanych shaderów
  SM6 - używany przy kompilacji shaderów z góry.
- DagorEngine (https://github.com/GaijinEntertainment/DagorEngine, BSD-3-Clause):
  układ pamięci potoków DirectX 12 (`cache\dx12.cache`), zrzutu shaderów i
  root signature - do odtwarzania potoków graficznych gier na Dagorze.
- Valve Developer Community (https://developer.valvesoftware.com/wiki/VPK):
  format archiwów VPK, do czytania paczek shaderów Source 2.

## oozextract (crate Rust)

- Źródło: https://github.com/lvlvllvlvllvlvl/oozextract
- Licencja: MIT (tak opublikowana; to port do Rust programu ooz autorstwa
  powzix, rozpowszechnianego na GPL)
- Rozpakowuje dane Oodle (Kraken / Mermaid / Selkie / Leviathan) w paczkach
  gier Unreal Engine, gdy gra nie ma biblioteki `oo2core_*.dll`.
