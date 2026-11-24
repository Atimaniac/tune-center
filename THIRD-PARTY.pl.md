*English: [THIRD-PARTY.md](THIRD-PARTY.md)*

# Składniki innych autorów

Tune Center korzysta z poniższych składników. Są dołączane bez zmian.

## PawnIO.Modules - IntelMSR.bin, RyzenSMU.bin, AMDFamily17.bin

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

## LibreHardwareMonitorLib

- Źródło: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- Licencja: MPL-2.0
- Używany przez `sensor-bridge.exe` do odczytu czujników i sterowania
  wentylatorami.

## Audiowide (czcionka interfejsu)

- Plik: `assets/Audiowide-Regular.ttf`, wbudowany w `tunecenter.exe`
- Autor: Astigmatic (Brian J. Bonislawsky)
- Licencja: SIL Open Font License 1.1

## Odniesienia użyte przy pisaniu kodu

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
- LibreHardwareMonitor (MPL-2.0): układ struktur NVAPI do odczytu i
  sterowania wentylatorami karty (ClientFanCoolers) oraz czujnika hotspot.
- AMD Display Library SDK (https://github.com/GPUOpen-LibrariesAndSDKs/display-library):
  struktury i wywołania Overdrive 8 dla kart Radeon.
- NVIDIA NVAPI SDK (https://github.com/NVIDIA/nvapi, MIT): identyfikatory
  funkcji, struktury i wartości ustawień sterownika (`nvapi.h`,
  `nvapi_interface.h`, `NvApiDriverSettings.h`) oraz przykłady
  DisplayConfiguration, DisplayColorControl i CustomTiming - podstawa
  zakładki NVIDIA. Kod SDK nie jest dołączany; program woła sterownik przez
  `nvapi64.dll`.
- AMD ADLX SDK (https://github.com/GPUOpen-LibrariesAndSDKs/ADLX): układ
  interfejsów C (`SDK/Include`) dla zakładki RADEON. Program woła sterownik
  przez `amdadlx64.dll`.
- Intel Graphics Control Library (https://github.com/intel/drivers.gpu.control-library,
  licencja Intel Software License Agreement, Copyright (C) Intel Corporation):
  struktury i funkcje z `include/igcl_api.h` dla zakładki INTEL. Nagłówek ani
  biblioteka nie są dołączane; program woła bibliotekę sterownika Intela
  `ControlLib.dll`, wyłącznie na platformach Intela.
- CUE4Parse (https://github.com/FabianFG/CUE4Parse, Apache-2.0): układ paczek
  Unreal Engine `.pak` / IoStore i archiwów shaderów (`.ushaderbytecode`)
  używany przy kompilacji shaderów z góry.

## oozextract (crate Rust)

- Źródło: https://github.com/lvlvllvlvllvlvl/oozextract
- Licencja: MIT (tak opublikowana; to port do Rust programu ooz autorstwa
  powzix, rozpowszechnianego na GPL)
- Rozpakowuje dane Oodle (Kraken / Mermaid / Selkie / Leviathan) w paczkach
  gier Unreal Engine, gdy gra nie ma biblioteki `oo2core_*.dll`.
