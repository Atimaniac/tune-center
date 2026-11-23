# Polityka prywatności

Ostatnia aktualizacja: 23 listopada 2026

Tune Center to darmowy program o otwartym kodzie dla systemu Windows,
publikowany przez Atimaniac pod adresem
https://github.com/Atimaniac/tune-center. Ta polityka opisuje, co program
robi z danymi na Twoim komputerze.

## W skrócie

- Tune Center nie ma telemetrii, analityki, reklam ani kont użytkowników.
- Autor nie otrzymuje, nie zbiera i nie przechowuje żadnych danych z programu.
- Program łączy się z internetem wyłącznie po to, żeby sprawdzić i pobrać
  aktualizacje. Oba sprawdzania można wyłączyć.
- Ustawienia, raporty i dzienniki zostają na Twoim komputerze, dopóki sam ich
  nie wyślesz.

## Dane, które zostają na komputerze

Tune Center odczytuje informacje o sprzęcie i systemie, bo tego wymaga jego
działanie: odczyty czujników, obroty wentylatorów, parametry procesora i karty
graficznej, dyski, urządzenia RGB, ustawienia systemu oraz ścieżki
uruchomionych procesów (żeby przełączyć profil wentylatorów po uruchomieniu
gry). Te informacje są używane lokalnie i nie są nigdzie wysyłane.

Program zapisuje w swoim folderze następujące pliki:

| Plik | Zawartość |
| --- | --- |
| `tunecenter-config.json`, `gpu-*.json` | Twoje ustawienia i profile, w tym foldery gier, którym przypisałeś profil |
| `tunecenter-crash.log`, `sensor-bridge-crash.log` | Raporty awarii: wersja programu, czas, kod błędu i moduł, który zawiódł |
| `tunecenter-diagnostics.txt` | Raport zgodności, tworzony tylko po uruchomieniu diagnostyki zgodności |

Historia odczytów na wykresach obejmuje ostatnie 120 sekund i jest trzymana
wyłącznie w pamięci.

## Połączenia z internetem

| Kiedy | Dokąd | Co jest wysyłane |
| --- | --- | --- |
| Sprawdzanie aktualizacji programu: przy starcie i co 15 minut działania programu | `api.github.com` | Zapytanie o najnowsze wydanie tego repozytorium z nagłówkiem `User-Agent: TuneCenter` |
| Pobranie aktualizacji programu: po kliknięciu instalacji | `github.com` | Żądanie pliku instalatora |
| Sprawdzanie sterownika NVIDIA: około 30 sekund po starcie i co 6 godzin | `www.nvidia.com`, `gfwsl.geforce.com` | Zapytanie o listę produktów NVIDIA, a potem identyfikator modelu Twojej karty graficznej i rodzina wersji Windows |
| Pobranie sterownika NVIDIA: po kliknięciu instalacji | `download.nvidia.com` | Żądanie paczki sterownika |
| Instalacja, tylko dla brakujących składników | źródła `winget` firmy Microsoft | Żądania instalacji .NET Runtime oraz, jeśli go zaznaczysz, PawnIO |

Jak przy każdym połączeniu z internetem, te serwery widzą Twój adres IP.
Zapytania nie zawierają nazwy użytkownika, nazwy komputera, numerów seryjnych
ani innych identyfikatorów. GitHub, NVIDIA i Microsoft przetwarzają te
zapytania według własnych polityk prywatności:

- GitHub: https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement
- NVIDIA: https://www.nvidia.com/en-us/about-nvidia/privacy-policy/
- Microsoft: https://privacy.microsoft.com/privacystatement

Oba sprawdzania wyłączysz w OPTIONS. Po wyłączeniu obu program nie nawiązuje
żadnych połączeń z internetem.

## Raporty zgodności i awarii

Diagnostyka zgodności wyłącznie odczytuje dane i zapisuje plik
`tunecenter-diagnostics.txt`. Raport zawiera informacje o sprzęcie i
sterownikach (modele, wersje, wartości rejestrów, kody błędów, wykryte gry).
Nie zawiera numerów seryjnych ani adresów MAC, a nazwa użytkownika Windows i
nazwa komputera są zastępowane znacznikami.

Nic nie jest wysyłane automatycznie. Przycisk zgłoszenia na GitHubie kopiuje
raport do schowka i otwiera formularz zgłoszenia w przeglądarce; raport
zostaje opublikowany tylko wtedy, gdy go wkleisz i wyślesz. Zgłoszenia na
GitHubie są publiczne i powiązane z Twoim kontem GitHub, więc przeczytaj
raport przed wysłaniem. To samo dotyczy dzienników awarii dołączanych do
zgłoszenia.

## Składniki innych autorów

Tune Center korzysta ze sterownika PawnIO, biblioteki LibreHardwareMonitor
oraz sterowników NVIDIA i AMD zainstalowanych w systemie. Ich lista jest w
pliku [THIRD-PARTY.pl.md](THIRD-PARTY.pl.md); podlegają one własnym warunkom.

## Dzieci

Tune Center nie jest kierowany do dzieci i nie zbiera danych od nikogo.

## Zmiany

Zmiany tej polityki są publikowane w tym pliku w repozytorium, z nową datą
na górze.

## Kontakt

Pytania dotyczące tej polityki: utwórz zgłoszenie pod adresem
https://github.com/Atimaniac/tune-center/issues.
