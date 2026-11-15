// sensor-bridge: cienka warstwa nad LibreHardwareMonitor.
// stdout: co sekundę jedna linia JSON ze stanem czujników.
// stdin:  set <id> <procent> | auto <id> | quit
// Przy wyjściu (quit, zamknięcie stdin, zakończenie procesu) wszystkie
// wentylatory wracają pod sterowanie płyty głównej.

using System.Collections.Concurrent;
using System.Globalization;
using System.Security.Principal;
using System.Text.Json;
using LibreHardwareMonitor.Hardware;

var stdout = Console.Out;
var restored = false;
var gate = new object();

Computer? computer = null;

void Restore()
{
    lock (gate)
    {
        if (restored || computer is null) return;
        restored = true;
        try
        {
            foreach (var hw in AllHardware(computer))
                foreach (var s in hw.Sensors)
                    if (s.SensorType == SensorType.Control)
                        s.Control?.SetDefault();
            computer.Close();
        }
        catch { /* nic więcej nie da się zrobić */ }
    }
}

try
{
    var isAdmin = new WindowsPrincipal(WindowsIdentity.GetCurrent())
        .IsInRole(WindowsBuiltInRole.Administrator);

    computer = new Computer
    {
        IsCpuEnabled = true,
        IsGpuEnabled = true,
        IsMotherboardEnabled = true,
        IsControllerEnabled = true,
        IsStorageEnabled = true,
        IsMemoryEnabled = true,
    };
    computer.Open();

    AppDomain.CurrentDomain.ProcessExit += (_, _) => Restore();
    Console.CancelKeyPress += (_, _) => Restore();

    var queue = new BlockingCollection<string>();
    var reader = new Thread(() =>
    {
        string? line;
        while ((line = Console.ReadLine()) != null) queue.Add(line);
        queue.Add("quit"); // rodzic zniknął -> kończymy i oddajemy sterowanie
    })
    { IsBackground = true };
    reader.Start();

    var running = true;
    while (running)
    {
        Emit(computer, isAdmin);

        var deadline = DateTime.UtcNow.AddSeconds(1);
        while (running)
        {
            var remaining = deadline - DateTime.UtcNow;
            if (remaining <= TimeSpan.Zero) break;
            if (queue.TryTake(out var cmd, remaining))
                running = Handle(computer, cmd);
        }
    }
}
catch (Exception ex)
{
    stdout.WriteLine(JsonSerializer.Serialize(new { error = ex.Message }));
    stdout.Flush();
}
finally
{
    Restore();
}

static IEnumerable<IHardware> AllHardware(Computer computer)
{
    foreach (var hw in computer.Hardware)
        foreach (var h in Walk(hw))
            yield return h;
}

static IEnumerable<IHardware> Walk(IHardware hw)
{
    yield return hw;
    foreach (var sub in hw.SubHardware)
        foreach (var h in Walk(sub))
            yield return h;
}

static float? Clean(float? v) =>
    v.HasValue && !float.IsNaN(v.Value) && !float.IsInfinity(v.Value) ? v : null;

void Emit(Computer computer, bool isAdmin)
{
    var temps = new List<object>();
    var fans = new List<object>();
    var speeds = new List<object>();
    var powers = new List<object>();

    foreach (var hw in AllHardware(computer))
    {
        hw.Update();

        foreach (var s in hw.Sensors)
        {
            if (s.SensorType == SensorType.Temperature)
            {
                temps.Add(new
                {
                    id = s.Identifier.ToString(),
                    name = s.Name,
                    hw = hw.Name,
                    kind = hw.HardwareType.ToString(),
                    value = Clean(s.Value),
                });
            }
        }

        // Pobór mocy (W): procesor, karty graficzne.
        foreach (var s in hw.Sensors)
        {
            if (s.SensorType != SensorType.Power) continue;
            powers.Add(new
            {
                name = s.Name,
                hw = hw.Name,
                kind = hw.HardwareType.ToString(),
                value = Clean(s.Value),
            });
        }

        // Wszystkie czujniki obrotów, także bez możliwości sterowania.
        foreach (var f in hw.Sensors)
        {
            if (f.SensorType != SensorType.Fan) continue;
            var ctl = hw.Sensors.FirstOrDefault(
                c => c.SensorType == SensorType.Control && c.Index == f.Index);
            speeds.Add(new
            {
                name = f.Name,
                hw = hw.Name,
                rpm = Clean(f.Value),
                percent = Clean(ctl?.Value),
            });
        }

        foreach (var c in hw.Sensors)
        {
            if (c.SensorType != SensorType.Control || c.Control is null) continue;

            // Czujnik obrotów o tym samym indeksie na tym samym układzie.
            var fan = hw.Sensors.FirstOrDefault(
                f => f.SensorType == SensorType.Fan && f.Index == c.Index);

            fans.Add(new
            {
                id = c.Identifier.ToString(),
                name = fan?.Name ?? c.Name,
                hw = hw.Name,
                kind = hw.HardwareType.ToString(),
                rpm = Clean(fan?.Value),
                percent = Clean(c.Value),
            });
        }
    }

    stdout.WriteLine(JsonSerializer.Serialize(new { temps, fans, speeds, powers, admin = isAdmin }));
    stdout.Flush();
}

static bool Handle(Computer computer, string line)
{
    // TrimStart('﻿'): niektóre procesy nadrzędne (np. .NET Framework)
    // poprzedzają pierwszą linię znacznikiem BOM, przez co "quit" nie byłby rozpoznany.
    var parts = line.TrimStart('﻿').Trim().Split(' ', StringSplitOptions.RemoveEmptyEntries);
    if (parts.Length == 0) return true;
    if (parts[0] == "quit") return false;
    if (parts.Length < 2) return true;

    var sensor = AllHardware(computer)
        .SelectMany(h => h.Sensors)
        .FirstOrDefault(s => s.SensorType == SensorType.Control
                             && s.Identifier.ToString() == parts[1]);
    if (sensor?.Control is null) return true;

    switch (parts[0])
    {
        case "set" when parts.Length >= 3 &&
                        float.TryParse(parts[2], NumberStyles.Float,
                            CultureInfo.InvariantCulture, out var pct):
            sensor.Control.SetSoftware(Math.Clamp(pct, 0f, 100f));
            break;
        case "auto":
            sensor.Control.SetDefault();
            break;
    }
    return true;
}
