// sensor-bridge: cienka warstwa nad LibreHardwareMonitor.
// stdout: co sekundę jedna linia JSON ze stanem czujników.
// stdin:  set <id> <procent> | auto <id> | quit
// Przy wyjściu (quit, zamknięcie stdin, zakończenie procesu) wszystkie
// wentylatory wracają pod sterowanie płyty głównej.

using System.Collections.Concurrent;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text.Json;
using LibreHardwareMonitor.Hardware;

// Bez okna "sensor-bridge przestał działać": gdy biblioteka sterownika (np.
// NVML w LibreHardwareMonitor) spowoduje naruszenie pamięci, proces po prostu
// kończy się, a Tune Center uruchamia go ponownie.
NativeMethods.SetErrorMode(NativeMethods.SEM_FAILCRITICALERRORS | NativeMethods.SEM_NOGPFAULTERRORBOX);

var stdout = Console.Out;
var restored = false;
var gate = new object();

Computer? computer = null;
// Czas ostatniego odczytu każdego podzespołu (patrz Interval).
var lastUpdate = new Dictionary<string, DateTime>();

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
        // Zasilacze z USB (Corsair, NZXT, Seasonic...): temperatury i obroty
        // wentylatora tylko do odczytu (sterowanie wyłączone niżej).
        IsPsuEnabled = true,
        // Bateria laptopa: tempo rozładowania = pobór całego komputera.
        IsBatteryEnabled = true,
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
    // Czy układ płyty głównej dał choć jedną temperaturę (patrz Acpi).
    var boardTemp = false;

    var now = DateTime.UtcNow;
    foreach (var hw in AllHardware(computer))
    {
        // Wolno zmieniające się podzespoły rzadziej (pomiar: karta w LHM
        // 63 ms / 31 ms CPU na odczyt - Tune Center czyta ją też wprost ze
        // sterownika co sekundę; dyski 9 ms; moduły RAM po 3 ms przez SMBus).
        // Między odczytami czujniki zachowują ostatnią wartość.
        var key = hw.Identifier.ToString();
        if (!lastUpdate.TryGetValue(key, out var last) || now - last >= Interval(hw.HardwareType))
        {
            hw.Update();
            lastUpdate[key] = now;
        }

        foreach (var s in hw.Sensors)
        {
            if (s.SensorType == SensorType.Temperature)
            {
                if ((hw.HardwareType is HardwareType.SuperIO or HardwareType.Motherboard) && Clean(s.Value) is not null)
                    boardTemp = true;
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
            // Wentylator zasilacza: tylko odczyt - jego chłodzeniem steruje zasilacz.
            if (hw.HardwareType == HardwareType.Psu) continue;

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

    // Układ czujników płyty, którego biblioteka nie odczytuje (np. NCT6683D z
    // firmware producenta): temperatury stref termicznych ACPI z Windows, z
    // wyraźnym źródłem "ACPI".
    if (!boardTemp)
    {
        foreach (var (name, celsius) in Acpi.Zones(now))
        {
            temps.Add(new
            {
                id = "/acpi/" + name,
                name = "ACPI " + name,
                hw = "ACPI",
                kind = "Motherboard",
                value = (float?)celsius,
            });
        }
    }

    stdout.WriteLine(JsonSerializer.Serialize(new { temps, fans, speeds, powers, admin = isAdmin }));
    stdout.Flush();
}

static TimeSpan Interval(HardwareType type) => type switch
{
    HardwareType.GpuNvidia or HardwareType.GpuAmd or HardwareType.GpuIntel => TimeSpan.FromSeconds(5),
    HardwareType.Memory => TimeSpan.FromSeconds(5),
    HardwareType.Storage => TimeSpan.FromSeconds(10),
    // Procesor, płyta główna (Super I/O - wentylatory), kontrolery: co sekundę.
    _ => TimeSpan.Zero,
};

static bool Handle(Computer computer, string line)
{
    // TrimStart('﻿'): niektóre procesy nadrzędne (np. .NET Framework)
    // poprzedzają pierwszą linię znacznikiem BOM, przez co "quit" nie byłby rozpoznany.
    var parts = line.TrimStart('﻿').Trim().Split(' ', StringSplitOptions.RemoveEmptyEntries);
    if (parts.Length == 0) return true;
    if (parts[0] == "quit") return false;
    if (parts.Length < 2) return true;

    // Wentylator zasilacza nie przyjmuje poleceń (tylko odczyt).
    var sensor = AllHardware(computer)
        .Where(h => h.HardwareType != HardwareType.Psu)
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

/// <summary>Strefy termiczne ACPI (licznik wydajności Windows, bez uprawnień
/// administratora): nazwa i temperatura w °C, odczyt co 10 s. Odrzucane są
/// wartości spoza 1..110 °C, a strefa pokazuje się dopiero, gdy jej wartość
/// zmieni się o co najmniej 0,5 °C - wiele płyt podaje tu stałą atrapę z BIOS-u
/// (np. 301 K = 27,9 °C), która nie jest pomiarem.</summary>
static class Acpi
{
    static DateTime last = DateTime.MinValue;
    static List<(string, float)> zones = new();
    /// <summary>Pierwsza wartość strefy i czy już się zmieniła (żywy czujnik).</summary>
    static readonly Dictionary<string, (float First, bool Live)> seen = new();

    public static List<(string, float)> Zones(DateTime now)
    {
        if (now - last < TimeSpan.FromSeconds(10)) return zones;
        last = now;
        var list = new List<(string, float)>();
        try
        {
            using var searcher = new System.Management.ManagementObjectSearcher(
                @"root\CIMV2", "SELECT Name, Temperature FROM Win32_PerfFormattedData_Counters_ThermalZoneInformation");
            foreach (var o in searcher.Get())
            {
                var kelvin = Convert.ToDouble(o["Temperature"], CultureInfo.InvariantCulture);
                var celsius = (float)Math.Round(kelvin - 273.15, 1);
                var name = (o["Name"] as string ?? "TZ").Split('.').Last();
                if (celsius is <= 1f or >= 110f) continue;
                var s = seen.TryGetValue(name, out var v) ? v : (celsius, false);
                if (Math.Abs(celsius - s.Item1) >= 0.5f) s = (s.Item1, true);
                seen[name] = s;
                if (s.Item2) list.Add((name, celsius));
            }
        }
        catch { /* brak licznika albo WMI - bez stref */ }
        zones = list;
        return zones;
    }
}

static class NativeMethods
{
    public const uint SEM_FAILCRITICALERRORS = 0x0001;
    public const uint SEM_NOGPFAULTERRORBOX = 0x0002;

    [DllImport("kernel32.dll")]
    public static extern uint SetErrorMode(uint mode);
}
