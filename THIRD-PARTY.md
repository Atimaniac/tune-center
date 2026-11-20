# Third-party components

Tune Center uses the components below. They are included unchanged.

## PawnIO.Modules – IntelMSR.bin, RyzenSMU.bin, AMDFamily17.bin, SmbusI801.bin

- Source: https://github.com/namazso/PawnIO.Modules
- Author: namazso
- License: LGPL-2.1-or-later
- The files in `modules\` are the official, signed modules from the project's
  releases. Their source code (`*.p`) is available at the address above.

## PawnIO (driver and PawnIOLib.dll)

- Source: https://github.com/namazso/PawnIO, https://pawnio.eu
- License: GPL-2.0
- Not bundled; it is installed separately (the Tune Center installer can do
  it through winget).

## LibreHardwareMonitorLib

- Source: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- License: MPL-2.0
- Used by `sensor-bridge.exe` to read sensors and control fans.

## serialport (Rust crate)

- Source: https://github.com/serialport/serialport-rs
- License: MPL-2.0
- Serial port access for the Skydimo LED strip.

## Audiowide (interface font)

- File: `assets/Audiowide-Regular.ttf`, embedded in `tunecenter.exe`
- Author: Astigmatic (Brian J. Bonislawsky)
- License: SIL Open Font License 1.1

## References used while writing the code

No code was copied from these projects; they were used as documentation.

- PowerMonkey (https://github.com/psyq321/PowerMonkey, Apache-2.0): field
  layout and command numbers of the Intel OC mailbox (MSR 0x150).
- ZenStates-Core (https://github.com/irusanov/ZenStates-Core, GPL-3.0): Ryzen
  SMU command numbers, core mask encoding and core fuse addresses.
- ryzen_monitor (https://github.com/hattedsquirrel/ryzen_monitor) and rigstats
  (https://github.com/dvalfrid/rigstats): position of the PPT, TDC and EDC
  limits in the PM table of Ryzen 5000 and 9000.
- OpenRGB (https://gitlab.com/CalcProgrammer1/OpenRGB, GPL-2.0-or-later):
  Skydimo LED strip protocol ("Moni-A" query, model table, Adalight frame,
  keep-alive).
- LibreHardwareMonitor (MPL-2.0): NVAPI structure layouts for reading and
  controlling graphics card fans (ClientFanCoolers) and the hotspot sensor.
- AMD Display Library SDK (https://github.com/GPUOpen-LibrariesAndSDKs/display-library):
  Overdrive 8 structures and calls for Radeon cards.
- NVIDIA NVAPI SDK (https://github.com/NVIDIA/nvapi, MIT): function IDs,
  structures and driver setting values (`nvapi.h`, `nvapi_interface.h`,
  `NvApiDriverSettings.h`) and the DisplayConfiguration, DisplayColorControl
  and CustomTiming samples – the basis of the NVIDIA tab. The SDK code is not
  bundled; the program calls the driver through `nvapi64.dll`.
