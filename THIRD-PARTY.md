# Third-Party Components

Tune Center uses the following components. They are included as is.

## PawnIO.Modules - IntelMSR.bin, RyzenSMU.bin, AMDFamily17.bin

- Source: https://github.com/namazso/PawnIO.Modules
- Author: namazso
- License: LGPL-2.1-or-later
- Files in `modules\` are official, signed modules downloaded from the project's release. Their source code (`*.p`) is available at the above address.

## PawnIO (driver and PawnIOLib.dll)

- Source: https://github.com/namazso/PawnIO, https://pawnio.eu
- License: GPL-2.0
- Not included; installed separately (the Tune Center installer can do this via winget).

## LibreHardwareMonitorLib

- Source: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
- License: MPL-2.0
- Used by `sensor-bridge.exe` to read sensors and control fans.

## Audiowide (interface font)

- File: `assets/Audiowide-Regular.ttf`, built into `tunecenter.exe`
- Author: Astigmatic (Brian J. Bonislawsky)
- License: SIL Open Font License 1.1

## References used in writing the code

- PowerMonkey (https://github.com/psyq321/PowerMonkey, Apache-2.0): Field layout

and command numbers for the Intel OC box (MSR 0x150). - ZenStates-Core (https://github.com/irusanov/ZenStates-Core, GPL-3.0): Ryzen processor SMU command numbers, core mask encoding, and fuse addresses
with core map. Used for reference; the code was not copied.
- ryzen_monitor (https://github.com/hattedsquirrel/ryzen_monitor) and rigstats (https://github.com/dvalfrid/rigstats): Location of the PPT, TDC, and EDC limits
in the PM table for Ryzen 5000 and 9000 processors.
- AMD Display Library SDK (https://github.com/GPUOpen-LibrariesAndSDKs/display-library): Overdrive 8 structures and calls for Radeon cards.
