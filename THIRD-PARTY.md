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
- LibreHardwareMonitor (MPL-2.0): układ struktur NVAPI do odczytu i
  sterowania wentylatorami karty (ClientFanCoolers) oraz czujnika hotspot.
- AMD Display Library SDK (https://github.com/GPUOpen-LibrariesAndSDKs/display-library):
  struktury i wywołania Overdrive 8 dla kart Radeon.
