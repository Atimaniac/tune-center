# Third-party components

Tune Center uses the components below. They are included unchanged.

## PawnIO.Modules – IntelMSR.bin, RyzenSMU.bin, AMDFamily17.bin, SmbusI801.bin, SmbusPIIX4.bin, LpcIO.bin

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

## LibreHardwareMonitor – motherboard sensor tables

- Source: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- License: MPL-2.0
- `src/hw/boards.rs` contains the motherboard tables from its
  `SuperIOHardware.cs` and `Identification.cs` (temperature, fan and fan
  control names for specific boards). That file stays under MPL-2.0; the
  library itself is not used or bundled.

## Inter (interface font)

- Files: `assets/Inter-Regular.ttf`, `assets/Inter-SemiBold.ttf` and
  `assets/Inter-BoldItalic.ttf` (Inter 4.1), embedded in `tunecenter.exe`
- Source: https://github.com/rsms/inter
- Author: The Inter Project Authors
- License: SIL Open Font License 1.1

## References used while writing the code

No code was copied from these projects; they were used as documentation.

- LibreHardwareMonitor (https://github.com/LibreHardwareMonitor/LibreHardwareMonitor,
  MPL-2.0): Super I/O register layouts (Nuvoton, ITE, Fintek), CPU and memory
  sensor sources; sensor names and identifiers are kept compatible so saved fan
  settings keep working.
- RAMSPDToolkit (https://github.com/Blacktempel/RAMSPDToolkit): SPD layout of
  DDR4/DDR5 thermal sensors.
- PowerMonkey (https://github.com/psyq321/PowerMonkey, Apache-2.0): field
  layout and command numbers of the Intel OC mailbox (MSR 0x150).
- ZenStates-Core (https://github.com/irusanov/ZenStates-Core, GPL-3.0): Ryzen
  SMU command numbers, core mask encoding and core fuse addresses.
- ryzen_monitor (https://github.com/hattedsquirrel/ryzen_monitor) and rigstats
  (https://github.com/dvalfrid/rigstats): position of the PPT, TDC and EDC
  limits in the PM table of Ryzen 5000 and 9000.
- OpenRGB (https://gitlab.com/CalcProgrammer1/OpenRGB, GPL-2.0-or-later):
  lighting protocol facts - Skydimo strip ("Moni-A" query, model table,
  Adalight frame), ENE RAM registers, Logitech HID++ RGB features,
  ASUS Aura USB, Gigabyte RGB Fusion 2, Corsair Lighting Node and MSI Mystic
  Light packet layouts and command numbers.
- NVIDIA NVAPI SDK (https://github.com/NVIDIA/nvapi, MIT): function IDs,
  structures and driver setting values (`nvapi.h`, `nvapi_interface.h`,
  `NvApiDriverSettings.h`) and the DisplayConfiguration and DisplayColorControl
  samples – the basis of the NVIDIA tab. The SDK code is not
  bundled; the program calls the driver through `nvapi64.dll`.
- AMD ADLX SDK (https://github.com/GPUOpen-LibrariesAndSDKs/ADLX): C interface
  layouts (`SDK/Include`) for the RADEON tab and the Radeon clocks, fans and sensors. The program calls the driver
  through `amdadlx64.dll`.
- Intel Graphics Control Library (https://github.com/intel/drivers.gpu.control-library,
  Intel Software License Agreement, Copyright (C) Intel Corporation):
  structures and functions of `include/igcl_api.h` for the INTEL tab. The
  header and the library are not bundled; the program calls the Intel driver's
  `ControlLib.dll`, on Intel platforms only.
- CUE4Parse (https://github.com/FabianFG/CUE4Parse, Apache-2.0): layout of
  Unreal Engine `.pak` / IoStore containers and shader code archives
  (`.ushaderbytecode`), DirectX 12 pipeline caches (`.upipelinecache`) and
  packed SM6 shaders used by shader pre-compilation.
- DagorEngine (https://github.com/GaijinEntertainment/DagorEngine, BSD-3-Clause):
  layout of the DirectX 12 pipeline cache (`cache\dx12.cache`), shader dump
  and root signatures, used to rebuild graphics pipelines of Dagor games.
- Valve Developer Community (https://developer.valvesoftware.com/wiki/VPK):
  VPK archive format, used to read Source 2 shader packages.

## oozextract (Rust crate)

- Source: https://github.com/lvlvllvlvllvlvl/oozextract
- License: MIT (as published; it is a Rust port of ooz by powzix, which is
  distributed under the GPL)
- Decompresses Oodle (Kraken / Mermaid / Selkie / Leviathan) data in Unreal
  Engine game packages when the game does not ship `oo2core_*.dll`.
