# Privacy policy

Last updated: 23 November 2026

Tune Center is a free, open-source Windows program published by Atimaniac at
https://github.com/Atimaniac/tune-center. This policy describes what the
program does with data on your computer.

## Summary

- Tune Center has no telemetry, analytics, advertising or user accounts.
- The author does not receive, collect or store any data from the program.
- The program connects to the internet only to check for and download
  updates. Both checks can be turned off.
- Settings, reports and logs stay on your computer unless you send them
  yourself.

## Data that stays on your computer

Tune Center reads hardware and system information to do its job: sensor
readings, fan speeds, CPU and graphics card parameters, disks, RGB devices,
system settings and the paths of running processes (to switch fan profiles
when a game starts). This information is used locally and is not sent
anywhere.

The program saves these files in its own folder:

| File | Contents |
| --- | --- |
| `tunecenter-config.json`, `gpu-*.json` | Your settings and profiles, including the folders of games you assigned profiles to |
| `tunecenter-crash.log`, `sensor-bridge-crash.log` | Crash reports: program version, time, error code and the module that failed |
| `tunecenter-diagnostics.txt` | The compatibility report, created only when you run the compatibility check |

Sensor history shown in the charts covers the last 120 seconds and is kept
only in memory.

## Internet connections

| When | Where | What is sent |
| --- | --- | --- |
| Program update check: at startup and every 15 minutes while the program runs | `api.github.com` | A request for the latest release of this repository, with the header `User-Agent: TuneCenter` |
| Program update download: when you click install | `github.com` | A request for the installer file |
| NVIDIA driver check: about 30 seconds after startup and every 6 hours | `www.nvidia.com`, `gfwsl.geforce.com` | A request for the list of NVIDIA products, then the product identifier of your graphics card model and the Windows version family |
| NVIDIA driver download: when you click install | `download.nvidia.com` | A request for the driver package |
| Installation, only for missing components | Microsoft `winget` sources | Requests to install the .NET Runtime and, if you select it, PawnIO |

As with any internet connection, these servers see your IP address. The
requests contain no user name, computer name, serial numbers or other
identifiers. GitHub, NVIDIA and Microsoft process these requests under their
own privacy policies:

- GitHub: https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement
- NVIDIA: https://www.nvidia.com/en-us/about-nvidia/privacy-policy/
- Microsoft: https://privacy.microsoft.com/privacystatement

You can turn off both checks in OPTIONS. With both off, the program makes no
internet connections.

## Compatibility and crash reports

The compatibility check only reads data and writes
`tunecenter-diagnostics.txt`. The report contains hardware and driver details
(models, versions, register values, error codes, detected games). It contains
no serial numbers or MAC addresses, and your Windows user name and computer
name are replaced with placeholders.

Nothing is sent automatically. *Report on GitHub* copies the report to the
clipboard and opens the issue form in your browser; the report is published
only if you paste and submit it. Issues on GitHub are public and tied to your
GitHub account, so read the report before you submit it. The same applies to
crash logs you attach to an issue.

## Third-party components

Tune Center uses the PawnIO driver, LibreHardwareMonitor and the NVIDIA and
AMD drivers already on your system. They are listed in
[THIRD-PARTY.md](THIRD-PARTY.md) and are governed by their own terms.

## Children

Tune Center is not directed at children and collects no data from anyone.

## Changes

Changes to this policy are published in this file in the repository, with a
new date at the top.

## Contact

Questions about this policy: open an issue at
https://github.com/Atimaniac/tune-center/issues.
