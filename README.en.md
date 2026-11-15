# tune-center

*Polski (pełna wersja): [README.md](README.md)*

- **GPU** - Nvidia GTX/RTX and AMD Radeon graphics card clocks and core clock/voltage curve for Nvidia cards (formerly NVCurve).
- **CPU** - Intel: power limits, voltage offsets, and Core IccMax; AMD Ryzen: Curve Optimizer for each core and PBO limits.
- **FAN** - motherboard fan control: manual or temperature-dependent curve with profiles (formerly FanCtl).
- **SENSORS** - all temperature readings and fan speeds.
- **POWER** - current CPU and GPU power consumption, total, and a counter for the energy used in the current session.
- **OPTIONS** - launching from Windows, minimizing to the tray, and a light theme.
- **ABOUT** - program version, description, and components used.

# Requirements
- .NET SDK 8: https://dotnet.microsoft.com/download
- PawnIO driver (https://pawnio.eu) - required for newer versions

LibreHardwareMonitor to access the motherboard's Super I/O
- Nvidia or AMD Adrenalin driver (for the "Graphics Card" tab)

### AMD Ryzen
Supported Desktop Processors:

| Processor | Curve Optimizer | PBO Limits |
| --- | --- | --- |
| Ryzen 5000 (Zen 3) | yes, -30..+30 | yes |
| Ryzen 7000 (Zen 4) | yes, -50..+30 | yes |
| Ryzen 9000 (Zen 5) | yes, -50..+30 | yes |

### Intel
- **Power Limits**: Turbo Boost Power Max (PL1), Turbo Boost Short Power Max
(PL2), and Turbo Boost Power Time Window.
- **Voltage Offset** separately for P-cores, E-cores, and cache (Ring),
in the range of -250 to +50 mV.
- **Core IccMax** - core current limit (12th, 13th, and 14th generation processors).


### Fans

- **Auto (BIOS)** - the fan is controlled by the motherboard (default).
- **Manual** - constant control with the slider.
- **Curve** - control depends on the selected temperature sensor
(default: CPU); the curve points are dragged with the mouse.

### Sensors

The SENSORS tab shows only the most important temperatures: one main sensor each for the CPU, graphics card, motherboard, hard drives, and RAM (if supported by your hardware).

## Power

- Separate tiles for the processor and each graphics card: current
power consumption (W), average and maximum for the session, and energy used (Wh).
- **Total**: total power consumption, average and maximum, total energy,
session time, and the "Reset counter" button. The maximum total is the highest
instantaneous total, not the sum of individual maximums.
- The bar below the value shows the current consumption relative to the session maximum.
- The counter runs from program startup, even with the window minimized.
- Interruptions longer than 5 seconds (computer sleep) are not included.
- This is the consumption of the processor and graphics card based on their own sensors, not the entire computer; the motherboard, disks, fans, and power supply losses
are not included.
- Processor power requires administrator privileges and the PawnIO driver.
