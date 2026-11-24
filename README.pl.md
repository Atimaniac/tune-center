# Tune Center

*English: [README.md](README.md)*

Jedno okno z ośmioma zakładkami (Windows 10/11, 64-bit). Zakładki
przełącza się przyciskami w nagłówku, obok logo:

- **NVIDIA** - karta graficzna w podzakładkach:
  - **Overclock** - zegary kart Nvidia GTX/RTX i AMD Radeon oraz krzywa
    zegar rdzenia / napięcie dla kart Nvidia (dawny NVCurve, wcześniej
    zakładka GPU),
  - odpowiednik panelu sterowania Nvidia: ustawienia 3D (globalne i dla
    programów), tryb ekranu, kolor i HDR, G-SYNC, rozdzielczości
    niestandardowe.
- **CPU** - Intel: limity mocy, offsety napięcia i Core IccMax;
  AMD Ryzen: Curve Optimizer dla każdego rdzenia i limity PBO.
- **FAN** - sterowanie wentylatorami płyty głównej: ręcznie lub krzywą
  zależną od temperatury, z profilami, także automatycznie dla gier
  (dawny FanCtl).
- **SENSORS** - najważniejsze temperatury z wykresami z ostatnich 2 minut.
- **POWER** - aktualny pobór mocy procesora i karty graficznej, suma
  oraz licznik energii zużytej w bieżącej sesji.
- **TUNE** - ustawienia Windows wpływające na opóźnienia i DPC, z kopią
  oryginalnych wartości i przywracaniem.
- **RGB** - podświetlenie RAM, taśm Skydimo, urządzeń Logitech G, kart
  NVIDIA i (nietestowane) ASUS Aura, Gigabyte RGB Fusion 2, Corsair i MSI
  Mystic Light: jeden kolor, wyłączenie albo efekt urządzenia.
- **BENCH** - test wydajności procesora w stylu CPU-Z, stress test i test
  stabilności karty graficznej.
- **OPTIONS** - uruchamianie z Windows, minimalizowanie do zasobnika
  i motyw wyglądu: Tune (z opcją jasnego), Neon Green, Neon City (miasto nocą
  z postacią w nagłówku i ulicą na dole okna) i Crimson.
- **ABOUT** - wersja programu, opis, autor, odnośnik do GitHuba, nota
  licencyjna i użyte składniki.

Stan połączenia z czujnikami ("Czujniki: połączono") jest na pasku na
samym dole okna. W dalszej części opisu sekcje noszą polskie nazwy:
"Karta graficzna" to NVIDIA > Overclock, "Procesor" - CPU, "Wentylatory" - FAN,
"Czujniki" - SENSORS.

Interfejs i logika są w Rust (egui). Czujniki płyty głównej obsługuje mały
proces pomocniczy `sensor-bridge` oparty na bibliotece LibreHardwareMonitor,
a kartę graficzną - bezpośrednio biblioteki sterowników: `nvapi64.dll`
(Nvidia) i `atiadlxx.dll` (AMD).

## Wymagania

- Rust: https://rustup.rs (toolchain `stable-x86_64-pc-windows-msvc`,
  instalator poprosi o Visual Studio Build Tools)
- .NET SDK 8: https://dotnet.microsoft.com/download
- Sterownik PawnIO (https://pawnio.eu) - potrzebny nowszym wersjom
  LibreHardwareMonitor do dostępu do układu Super I/O płyty głównej
- Sterownik Nvidia lub AMD Adrenalin (dla zakładki "Karta graficzna")

## Budowanie i uruchamianie

W PowerShell, w katalogu projektu:

    .\build.ps1

Gotowy program trafia do `dist\`. Pliki `tunecenter.exe` i `sensor-bridge.exe`
muszą leżeć w tym samym katalogu. Uruchom `dist\tunecenter.exe`. Program działa bez uprawnień
administratora (karta graficzna, panel Nvidia, kompilacja shaderów, dyski);
administrator jest potrzebny do zakładki CPU, temperatury i mocy procesora,
wentylatorów płyty głównej, zmiany HAGS i autostartu - przycisk "Uruchom
jako administrator" w pasku stanu uruchamia program ponownie z pytaniem UAC.

Parametry:

- `--demo` - symulowana karta graficzna, nic nie trafia do sprzętu
  (dobre na pierwsze uruchomienie),
- `--minimized` - start zminimalizowany (używa go autostart).

## Instalator

1. Zainstaluj Inno Setup 6.3 lub nowszy: `winget install JRSoftware.InnoSetup`
2. Uruchom `.\build-installer.ps1`

Powstanie `installer-output\TuneCenter-Setup.exe`. Instalator:

- kopiuje program do `C:\Program Files\Tune Center` i tworzy skrót w menu Start,
- sprawdza, czy są .NET 11 Runtime i sterownik PawnIO; brakujące proponuje
  zainstalować (pola wyboru), pobierając je przez `winget`,
- podaje wydawcę "Atimaniac" (Programy i funkcje, właściwości pliku),
- usuwa wpis "zawsze jako administrator" ze starszych wersji,
- przy odinstalowaniu usuwa zadanie autostartu i pliki ustawień.

Sterownika Nvidia instalator nie instaluje.

### Aktualizacja

Uruchomienie nowszego instalatora na komputerze, na którym Tune Center już
jest, działa jako aktualizacja:

- na starcie pojawia się pytanie z wersją zainstalowaną i nową,
- kreator pokazuje "Gotowy do aktualizacji" i przycisk "Aktualizuj",
- działający program (także schowany w zasobniku) jest zamykany,
- pliki są podmieniane w tym samym katalogu, a ustawienia, profile
  wentylatorów, profile GPU i autostart zostają.

Przy tej samej wersji instalator proponuje ponowną instalację (naprawę),
a przy starszej ostrzega przed zastąpieniem nowszej.

Przed zbudowaniem nowego instalatora trzeba podnieść numer wersji w dwóch
miejscach: `version` w `Cargo.toml` i `AppVersion` w `installer.iss`.

## Karta graficzna (NVIDIA > Overclock)

Podzakładka Overclock jest pierwsza w zakładce NVIDIA i działa także na
kartach AMD oraz wtedy, gdy pozostałe podzakładki (panel Nvidia) są
niedostępne.

- **Informacje**: czujniki karty jak w GPU-Z - wiersz z nazwą, wartością
  (bieżąca, najmniejsza, największa albo średnia) i wykresem z 2 minut na
  całą szerokość (najechanie kursorem pokazuje wartość i czas odczytu):
  zegary GPU, pamięci i wideo, temperatura pamięci, hotspot (gdy karta go
  podaje - RTX 50 nie), zajęta pamięć, obciążenie GPU, silnika wideo i
  magistrali, enkoder, dekoder, napięcie. Przycisk "Ukryte" rozwija mniej
  istotne odczyty: PCIe TX/RX, obciążenie kontrolera pamięci, pobór mocy
  karty i układu GPU, % TDP, zajęta pamięć systemu. Nad wierszami: przyczyna
  ograniczenia zegarów, stan P, łącze PCIe, pamięć karty, TDP; poniżej aktywne
  offsety. Temperatura karty i procesora jest w SENSORS, wentylator w FAN.
  Źródła: NVML (oficjalne), NVAPI, Windows. Moc na złączu 16-pin i w slocie
  PCIe oraz ich napięcia nie są dostępne w publicznym API Nvidii.
- **Zegary**: offset rdzenia i pamięci; zakresy suwaków podaje sterownik.
- **Krzywa V/F**:
  - przeciągnij punkt w pionie, aby zmienić zegar przy danym napięciu,
  - Shift + przeciąganie przesuwa całą krzywą,
  - "Wyrównaj w prawo" ustawia wszystkie punkty na prawo od zaznaczonego
    (wyższe napięcie) na jego częstotliwość (undervolting),
  - po "Wyrównaj w prawo" obok przycisków widać "Limit napięcia: X mV".
    Po każdym "Zastosuj" program odczytuje krzywą z karty i obniża punkty
    o wyższym napięciu, które sterownik zaokrąglił powyżej zaznaczonego -
    inaczej karta sięgałaby po ich napięcie. Ręczna zmiana pojedynczego
    punktu wyłącza to pilnowanie,
  - "Ten sam offset wszędzie" nadaje wszystkim punktom offset zaznaczonego,
  - szara linia to krzywa fabryczna, żółty krzyż to bieżący punkt pracy.
- Zmiany trafiają do karty dopiero po kliknięciu **Zastosuj**. Zapis jest
  od razu odblokowany - nie ma już osobnego pola potwierdzenia.
- **Profile 1-5**: kliknij "Zapisz" - numery od 1 do 5 zaczną pulsować -
  i wybierz numer, pod którym zapisać bieżące ustawienia. Kliknięcie numeru
  poza trybem zapisu wczytuje profil (do karty trafia po "Zastosuj").
  Puste sloty są wyszarzone. Plik: `gpu-profiles.json` obok programu;
  pojedynczy profil ze starszej wersji trafia do slotu 1.
  Profile GPU są osobne od profili wentylatorów.
- **Uruchamiaj profil z Windows**: program pamięta ostatnio zapisany lub
  wczytany profil (`gpu-settings.json`) i po zaznaczeniu tego pola stosuje
  go przy każdym swoim uruchomieniu. Żeby działo się to przy starcie
  systemu, trzeba też włączyć "Uruchamiaj z Windows" w zakładce OPTIONS.
  Zabezpieczenie: jeśli po automatycznym zastosowaniu profilu program nie
  przepracuje minuty (zawieszenie albo restart komputera), przy następnym
  starcie opcja wyłącza się sama i karta zostaje na ustawieniach
  fabrycznych. Zwykłe zamknięcie programu nie jest liczone jako zawieszenie.
- **Eksportuj / Importuj**: zapisuje bieżące ustawienia do wybranego pliku
  `.json` i wczytuje je z powrotem, np. po ponownej instalacji programu lub
  systemu. Import wstawia ustawienia do edycji: "Zastosuj" wysyła je do
  karty, a "Zapisz" i numer zachowuje je w programie. Plik pamięta
  producenta karty i nie wczyta się na karcie drugiego producenta.
- Gdy program nie znajdzie karty Nvidia ani AMD, podzakładka działa w trybie demo.

## Panel Nvidia (zakładka NVIDIA)

Podzakładki poza Overclock są zbudowane wyłącznie na oficjalnym, publicznym NVAPI SDK
(https://github.com/NVIDIA/nvapi, licencja MIT) - bez funkcji
nieudokumentowanych. Działa niezależnie od trybu `--demo`.

- **DLSS** - nadpisania DLSS jak w NVIDIA App, globalnie albo dla wybranej
  gry (ten sam wybór profilu co w "Ustawieniach 3D"): Super Resolution
  (nadpisanie, model/preset - także "Najnowszy", tryb od Ultra wydajności
  po DLAA, własna skala renderowania 33-100%), Ray Reconstruction (to samo),
  Frame Generation (nadpisanie, model, tryb - także dynamiczny, mnożnik
  generowanych klatek, docelowy FPS) oraz DLAA, "Wydajność = Ultra
  wydajność" i nadpisanie w grach ze Streamline. Wszystkie ustawienia są w
  publicznym `NvApiDriverSettings.h` (SDK R620); zapis z weryfikacją odczytem
  i kopią zapasową jak w "Ustawieniach 3D". Zmiany działają od następnego
  uruchomienia gry.
- **Ustawienia 3D** - jak "Zarządzaj ustawieniami 3D": V-Sync (także
  adaptacyjna i szybka), maks. klatek przygotowanych, limit FPS,
  technologia monitora, preferowane odświeżanie, tryb zarządzania energią,
  filtrowanie tekstur i anizotropowe, antyaliasing, okluzja otoczenia i
  ustawienia VR.
  - "Ustawienia globalne" albo "Ustawienia programu": wybór z listy profili
    Nvidii (wyszukiwarka) lub "Dodaj program..." (plik .exe). W profilu
    programu ustawienie dziedziczone z globalnego ma dopisek "(globalne)",
    a przycisk "globalne" wraca do ustawienia globalnego.
  - Profile utworzone przez Ciebie (★) można usunąć; profile Nvidii tylko
    przywrócić do domyślnych.
  - "Zaawansowane" pokazuje surowe wartości wszystkich ustawień sterownika
    (ok. 130) z możliwością edycji wartości DWORD.
  - Zmiany są trwałe (jak w panelu Nvidia). Przed pierwszą zmianą w danym
    uruchomieniu programu powstaje kopia całej bazy ustawień w katalogu
    `nvidia-backup` obok programu; "Przywróć z kopii" wczytuje ją z powrotem.
    Każdy zapis jest sprawdzany ponownym odczytem.
- **Ekran** - rozdzielczość, częstotliwość odświeżania, obrót i skalowanie.
  Lista zawiera tryby przyjęte przez sterownik w teście bez zmiany obrazu,
  ograniczone do natywnej rozdzielczości i maksymalnego odświeżania z EDID
  monitora.
- **Kolor i HDR** - "Domyślne ustawienia koloru" albo ustawienia Nvidia:
  format (RGB / YCbCr444 / 422 / 420), głębia (6-12 bitów), zakres
  dynamiczny (pełny / ograniczony); niedostępne połączenia są wyszarzone.
  HDR10 włącz/wyłącz, gdy monitor go obsługuje.
- **G-SYNC** - tryb G-SYNC (pełny ekran / pełny ekran i okno), wskaźnik na
  ekranie, G-SYNC Compatible dla monitora i bieżący stan VRR.
- **OLED fix** - ukryta wartość sterownika `vrrFrameSplitBehavior` w kluczu
  karty (`HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-…}\000N`).
  0x80010055 = najniższy mnożnik klatek przy VRR (1× aż do 48 FPS zamiast
  podwajania np. 60 FPS → 120 Hz, które na OLED daje migotanie jasności);
  0x80000032 = zachowanie NVIDIA, ale rzadsze przełączanie. „Wyłącz” usuwa
  wpis. Działa po restarcie; program przywraca wpis po czystej instalacji
  sterownika. Wymaga administratora.
- **Rozdzielczości niestandardowe** - lista, usuwanie i dodawanie (taktowanie
  CVT z ograniczonym wygaszaniem liczy sterownik).

Zmiany ekranu (tryb, kolor, HDR, nowa rozdzielczość) działają najpierw na
próbę: przez 15 s można je zachować albo cofnąć, a bez odpowiedzi wracają
same - także przy oknie schowanym w zasobniku i przy zamknięciu programu.

Czego nie ma, bo publiczne NVAPI tego nie udostępnia: Digital Vibrance,
jasność/kontrast/gamma pulpitu, ustawienia wideo, konfiguracja PhysX, DSR
i tryb niskich opóźnień "Ultra" (dostępne jest "maks. klatek przygotowanych
= 1", czyli "Wł.").

### Shader compilation (w budowie)

Kompilacja shaderów gier z góry, do pamięci podręcznej sterownika, żeby gra
nie musiała tego robić w trakcie rozgrywki ("shader stutter"). Nvidia nie
udostępnia pobierania gotowych shaderów; Tune Center czyta je z plików gry.

- **Teraz**: wyszukiwanie zainstalowanych gier (Steam, Epic Games, GOG,
  Ubisoft Connect, EA app, Xbox), rozpoznanie silnika i jego wersji, API
  grafiki (z logu Unity / konfiguracji Dagora), anty-cheatu i plików
  shaderów; rozmiar pamięci podręcznej shaderów sterownika (oficjalne
  ustawienie DRS) i zajętość folderu `%LOCALAPPDATA%\NVIDIA\DXCache`.
- **Etap 2 (gotowy)**: Dagor Engine DirectX 12 - przycisk
  "Kompiluj shadery" w szczegółach gry kompiluje shadery obliczeniowe
  (compute) oraz potoki graficzne odtworzone z pamięci potoków gry
  (`cache\dx12.cache`, powstaje po pierwszej sesji w DX12): pary VS/PS,
  topologia, układ wejścia, formaty celów renderowania i stan (blending,
  głębia, rasteryzacja). Active Matter: 5525 potoków graficznych. Kompiluje sterownik Nvidii przez
  Direct3D 12, w procesie nazwanym tak jak gra (pamięć podręczna sterownika
  jest przypisana do nazwy pliku .exe), z gniazdem rozszerzeń shaderów
  ustawionym jak w grze (`NvAPI_D3D12_SetNvShaderExtnSlotSpace`). Gra musi
  być zamknięta. Pierwsza kompilacja Active Matter: ok. 5-7 min; kolejna,
  z pamięci podręcznej, ok. 20 s.
- **Etap 3 (gotowy)**: Unity DirectX 11 - Tune Center wyszukuje w plikach
  gry (luźne pliki i paczki `.bundle`) wszystkie shadery DirectX 11 ze
  wszystkimi wariantami, a sterownik Nvidii kompiluje je przez Direct3D 11
  w procesie nazwanym jak gra, partiami (każda na nowym urządzeniu, z
  czekaniem na zakończenie kompilacji w sterowniku). Pillars of Eternity:
  2640 shaderów; Last Epoch: ok. 196 tys. shaderów, ok. 17 min.
- **Stan i kolejka (gotowe)**: kolumna „Stan” (Skompilowano dd.mm /
  Nowy sterownik - skompiluj ponownie / Gra zaktualizowana / Nie
  kompilowano), szczegóły ostatniej kompilacji (sterownik, API, liczby,
  czas, rozmiar pamięci podręcznej gry), przycisk „Kompiluj wszystkie
  wymagające” (kolejka - gry po kolei, uruchomione gry są pomijane) i
  opcja kompilacji w tle po instalacji nowego sterownika NVIDIA (sterownik
  czyści swoją pamięć podręczną). Wyniki w `shader-state.json`.
- **Unreal Engine DirectX 11 (gotowy)**: gry z klasyczną paczką `.pak`
  (wersja 10-11) bez szyfrowania. Tune Center czyta archiwa shaderów SM5
  (`ShaderArchive-*-PCD3D_SM5.ushaderbytecode`) z paczki, rozpakowuje Oodle
  (crate `oozextract`, albo `oo2core_*.dll` z gry) i kompiluje DXBC przez
  Direct3D 11 pod nazwą `*-Shipping.exe` gry. API gry z `DefaultEngine.ini`
  w paczce. Palworld: 56 690 shaderów, 5 min, ponownie 25 s. Gry z
  szyfrowanymi paczkami (np. The Finals) są oznaczone jako nieobsługiwane
  (Tune Center nie łamie zabezpieczeń gier). Gry z kontenerami IoStore
  (`.utoc`/`.ucas`) - biblioteki shaderów SM5 z kontenerów.
- **Unreal Engine DirectX 12**: gdy gra ma pamięć potoków
  (`*.upipelinecache` w paczce albo w `Saved`), Tune Center odtwarza z niej
  potoki graficzne i obliczeniowe: shadery SM6 z archiwów gry, root signature
  budowana tak jak w silniku (zależnie od wersji UE 5.x odczytanej z .exe),
  formaty i stan potoku. Potoki z mesh shaderami są pomijane.
- **Dagor Engine DirectX 11**: wszystkie shadery ze zrzutu
  `compiledShaders\game.ps50.shdump.bin` (kontenery DXBC) tą samą drogą co
  Unity. Active Matter: 8632 shadery, ok. 90 s.
- **Source 2 DirectX 11** (np. Counter-Strike 2): shadery DirectX z plików
  `.vcs` w paczkach VPK (`shaders*_dir.vpk`), także z ramek zstd.
- **Inne gry DirectX 11** (nierozpoznany silnik): próba - luźne pliki
  shaderów gry (`.cso`, `.fxo` itp.) przeszukiwane pod kątem kontenerów DXBC.
- **AMD i Intel**: sterowniki tych firm biorą pod uwagę także układ wejścia
  shadera wierzchołków, więc w DirectX 11 Tune Center tworzy go z sygnatury
  wejścia shadera (`ISGN`).
- **Raport** `tunecenter.exe --shader-report [fragment nazwy gry]` - tylko
  odczyt: zapisuje `tunecenter-shaders.txt` z tym, co program znajduje w
  plikach każdej gry (silnik, paczki, pamięci potoków, liczby shaderów).
- **"Wyczyść shadery"** (obok "Kompiluj shadery", z potwierdzeniem "Czy jesteś
  pewny, że chcesz wyczyścić?") usuwa skompilowane shadery gry z pamięci
  podręcznej sterownika Nvidia (`DXCache`) i Windows (`D3DSCache`, m.in.
  shadery DX12 skompilowane przez SCSKiller), dla DX11 i DX12. Nazwy plików w `DXCache`
  zawierają skrót nazwy .exe, którego algorytmu Nvidia nie publikuje, więc
  program ustala je pomiarem: kopia programu nazwana jak gra tworzy
  urządzenia D3D11 i D3D12, a pliki, które sterownik wtedy otworzył (i
  kolejne numery tego samego skrótu), należą do gry. Gra musi być zamknięta.
  Foldery `D3DSCache` (bazy SQLite, osobna dla każdej ścieżki .exe) są
  rozpoznawane po zapisanej w nich ścieżce programu - usuwane są wszystkie
  z nazwą exe gry: sama gra, kopie robocze SCSKiller i kompilacje Tune Center.
- **Wybór API (AUTO / DX11 / DX12)** w szczegółach gry. AUTO wybiera API, którego
  używa gra (konfiguracja gry albo jej log); DX11 i DX12 wymuszają dane API,
  o ile gra ma do niego shadery - pamięć podręczna sterownika jest osobna dla
  każdego API, więc kompilacja dla innego API niż gra pokazuje ostrzeżenie.
  Unity w DX12 jest niedostępne (potoki powstają dopiero w trakcie gry).
- Gry są tylko czytane: Tune Center niczego w nich nie zmienia, nie
  uruchamia ich i nie dodaje do nich plików (bezpieczne także przy
  anty-cheacie). Unity w DirectX 12 buduje potoki dopiero w trakcie gry,
  więc tego z samych plików zrobić się nie da.
- Auto Shader Compilation z NVIDIA App nie jest obsługiwane - wymaga
  niepublicznych funkcji NVAPI.

## Procesor

Program rozpoznaje producenta procesora i pokazuje tylko jego opcje: na
Ryzenie nie ma ustawień Intela i odwrotnie. Ustawienia trafiają do procesora
przez sterownik PawnIO i jego oficjalne moduły (pobierane przez `build.ps1`
do `dist\modules\`; przy nowszym wydaniu PawnIO.Modules moduły są pobierane
ponownie). Pola ustawień są od razu wypełnione wartościami odczytanymi z
procesora i można je zmieniać bez dodatkowego odblokowania.

### Monitor (jak w Intel XTU)

Na górze zakładki CPU (procesory Intel) są odczyty co sekundę - te same, które
pokazuje Intel XTU. XTU czyta je własnym sterownikiem (`iocbios2.sys`, lista
czujników w `IntelOverclockingSDK`), z rejestrów MSR opisanych w Intel SDM;
Tune Center czyta te same rejestry przez moduł PawnIO `IntelMSR.bin`:

| Odczyt | Rejestr |
| --- | --- |
| Temperatura pakietu, TjMax | 0x1B1, 0x1A2 |
| Temperatura każdego rdzenia | 0x19C (na danym rdzeniu) |
| Moc pakietu / rdzeni / grafiki | liczniki energii RAPL 0x611 / 0x639 / 0x641, jednostki 0x606 |
| Napięcie rdzenia (VID) | 0x198 |
| Zegar każdego rdzenia, maks. P i E | APERF/MPERF (0xE8/0xE7), zegar bazowy 0xCE |
| Zegar cache (ring) | 0x621 |
| Aktywne ograniczenia (temperatura, PL1, PL2, prąd/EDP, VR...) | 0x64F |

Wymaga administratora i PawnIO.

### AMD Ryzen

Obsługiwane procesory do komputerów stacjonarnych:

| Procesor | Curve Optimizer | Limity PBO |
| --- | --- | --- |
| Ryzen 3000 (Zen 2) | nie (procesor go nie ma) | tak, niesprawdzone na sprzęcie |
| Ryzen 5000 (Zen 3) | tak, -30..+30 | tak |
| Ryzen 7000 (Zen 4) | tak, -50..+30 | tak |
| Ryzen 9000 (Zen 5) | tak, -50..+30 | tak |

- **Curve Optimizer**: kafelek dla każdego rdzenia z bieżącą wartością
  i polem nowej. "Wpisz do wszystkich" wypełnia pola jedną wartością; do
  procesora trafiają dopiero po "Zastosuj Curve Optimizer". Przed zapisem
  program sprawdza, czy odczyt marginesu działa, a po zapisie porównuje
  odczyt z zadaną wartością.
- **Ryzen 5000**: zapis marginesu idzie przez skrzynkę MP1 (polecenie 0x35),
  a odczyt kontrolny przez RSMU (0x7C). Jeśli odczyt nie działa na danym
  procesorze, program niczego nie zapisuje.
- **Limity PBO**: PPT (W), TDC (A), EDC (A) i skalar PBO (1x-10x), każdy
  wysyłany osobnym przyciskiem. Kolumna "Obecnie" pokazuje wartości
  z tabeli procesora (tabela PM), a pola startują od nich.
- **Potwierdzanie limitów**: układ tabeli PM zależy od jej wersji.
  Potwierdzone układy to 0x380804, 0x380805 i 0x380904 (Ryzen 5000) oraz
  0x620105 (Ryzen 9000). Dla innych wersji wartość ma znak zapytania,
  dopóki nie ustawisz danego limitu: po zapisie program szuka zadanej
  wartości w tabeli i, jeśli leży w innym polu niż zakładane, poprawia
  odczyt. Gdy tabela nie pokazuje zadanej wartości, program mówi to wprost
  (płyta mogła ograniczyć limit).
- Mapa rdzeni pochodzi z procesora (w modelach 12-rdzeniowych numer rdzenia
  w systemie nie odpowiada wprost miejscu w układzie). Jeśli nie zgadza się
  z liczbą rdzeni widzianą przez system, Curve Optimizer jest wyłączany.
- Ustawienia działają do restartu lub uśpienia komputera. Zamknij Ryzen
  Master na czas zmian.
- Logika ma testy na symulowanym procesorze (18 testów). Na prawdziwym
  Ryzenie kod nie był uruchamiany.

### Intel

Co można zmieniać:

- **Limity mocy**: Turbo Boost Power Max (PL1), Turbo Boost Short Power Max
  (PL2) i Turbo Boost Power Time Window.
- **Offset napięcia** osobno dla rdzeni P, rdzeni E i cache (Ring),
  w zakresie -250..+50 mV.
- **Core IccMax** - limit prądu rdzeni (procesory 12., 13. i 14. generacji).
- **Mnożniki turbo** (jak "Active Core Ratio" / "Per-Core Tuning" w XTU):
  limit mnożnika rdzeni P (rejestr 0x1AD) i rdzeni E (0x650) dla każdej z
  8 grup aktywnych rdzeni. Zakres 8-80, przy większej liczbie aktywnych
  rdzeni mnożnik nie może być wyższy; grupy nieużywane (0) zostają bez zmian.
  Wymaga modułu IntelMSR.bin z PawnIO.Modules 0.2.12 lub nowszego oraz
  odblokowanego procesora (K) - zablokowany nie przyjmie zmian, a program
  przywróci poprzednie wartości.

Jak to działa:

- Pola są wypełnione wartościami odczytanymi z procesora; każda sekcja ma
  własny przycisk "Zastosuj".
- Przy każdym zapisie program odczytuje bieżącą wartość, podmienia tylko
  zmieniane pole, zapisuje i odczytuje ponownie. Jeśli procesor nie przyjął
  wartości, pojawia się błąd z tym, co zadano i co odczytano.
- Ustawienia działają do restartu komputera. Program nie zapisuje ich na
  stałe i nie stosuje przy starcie systemu - po zawieszeniu wystarczy restart.
- Zamknij Intel XTU i ThrottleStop; korzystają z tych samych rejestrów.

Czego nie ma i dlaczego:

- **Mnożnik przypisany do konkretnego rdzenia** (np. "rdzeń 3 = 55x" w
  XTU) - polecenia XTU dla pojedynczego rdzenia nie są publicznie opisane;
  Tune Center ustawia mnożniki według liczby aktywnych rdzeni (jak wyżej).
- **Voltage Override** (stałe napięcie) i tryb napięcia - tylko odczyt.
- Profile i automatyczne stosowanie ustawień procesora.

Uwagi:

- Jeśli BIOS ma włączoną blokadę (Overclocking Lock, Undervolt Protection,
  blokada limitów mocy), procesor odrzuci zapis i program pokaże błąd.
- Na części płyt obowiązuje dodatkowy limit mocy po stronie płyty; wtedy
  działa niższy z dwóch i podniesienie PL1/PL2 w programie nic nie zmieni.
- Układ poleceń dla napięcia i IccMax pochodzi z projektu PowerMonkey
  (Alder Lake). Raptor Lake korzysta z tej samej platformy, ale nie był
  osobno potwierdzony.
- Logika zapisu ma testy na symulowanym procesorze (16 testów). Na
  prawdziwym procesorze kod nie był uruchamiany.

### Karty AMD Radeon

- Obsługiwany jest interfejs Overdrive 8, czyli seria RX 5000 i nowsze.
  Starsze karty pokazują tylko odczyty w "Informacje".
- **Zegary**: zamiast offsetów ustawia się bezwzględną wartość maksymalnego
  zegara rdzenia i pamięci (tak działa sterownik AMD).
- **Krzywa V/F** jest niedostępna (wyszarzona).
- W zakładce **Wentylatory** dla wentylatorów karty AMD niedostępny jest
  tryb "Krzywa"; zostają "Auto" i "Ręcznie".
- Profil GPU zapisany dla karty jednego producenta nie wczyta się na karcie
  drugiego (offsety i wartości bezwzględne nie są zamienne).
- Zamknij AMD Software: Adrenalin na czas strojenia - może nadpisywać
  ustawienia.

## Wentylatory

- **Auto (BIOS)** - wentylatorem steruje płyta główna (domyślnie).
- **Ręcznie** - stałe wysterowanie suwakiem.
- **Krzywa** - wysterowanie zależne od wybranego czujnika temperatury
  (domyślnie procesor); punkty krzywej przeciąga się myszą.

**Profile:** zmiany zapisują się na bieżąco w aktywnym profilu. Aby utworzyć
kolejny (np. "Cichy", "Gry"), wpisz nazwę i kliknij "Zapisz jako nowy" -
powstanie kopia bieżących ustawień. Wszystko jest przechowywane
w `tunecenter-config.json` obok programu.

**Uruchamiaj z Windows** (pole w zakładce OPTIONS): tworzy zadanie `TuneCenter` w Harmonogramie zadań
Windows (przy logowaniu, z najwyższymi uprawnieniami, bez pytania UAC).
Program startuje wtedy zminimalizowany i od razu stosuje aktywny profil
wentylatorów. Po przeniesieniu programu do innego katalogu odznacz
i zaznacz pole ponownie.

### Profile gier

Karta „Profile gier” pokazuje gry wykryte w Steam, Epic, GOG, Ubisoft, EA
i Xbox; gry spoza launcherów dodaje się przyciskiem „Dodaj grę (.exe)”.
Każdej grze można przypisać profil wentylatorów („globalny” = bez zmiany).
Program co 2 s sprawdza uruchomione procesy (gra rozpoznawana po folderze
albo po wskazanym pliku .exe) i przełącza wentylatory na profil gry, a po
jej zamknięciu wraca do **profilu globalnego**. Przy kilku grach naraz
wygrywa ostatnio uruchomiona. Po starcie programu obowiązuje profil
globalny; „Edytowany profil” służy do zmiany ustawień dowolnego profilu.

### Puste złącza

Złącza płyty głównej, które pokazują 0 obr./min albo nie mają odczytu, są
ukryte, o ile działają w trybie Auto. Wentylatory karty graficznej są widoczne
zawsze (często stoją przy niskiej temperaturze). Ukryte złącza pokazuje
pole "Pokaż złącza bez wentylatora".

## Czujniki

Zakładka SENSORS pokazuje tylko najważniejsze temperatury: po jednym głównym
czujniku dla procesora, karty graficznej, płyty głównej, dysków i pamięci RAM
(jeśli sprzęt je udostępnia). Kafelki dzielą między siebie całe okno, więc
wszystkie są widoczne bez przewijania; liczba kolumn i wielkość cyfr zależą od
rozmiaru okna. Każdy kafelek ma plakietkę stanu (NORMALNA poniżej 65 °C,
WYSOKA od 65 °C, GORĄCO od 80 °C), zmianę z ostatnich 5 s, minimum i maksimum
oraz wykres z ostatnich 120 s (odczyt co sekundę, także gdy okno jest
schowane). Obroty wentylatorów są w zakładce FAN.

## Power

- Osobny kafelek dla procesora i dla każdej karty graficznej: aktualny
  pobór mocy (W), średnia i maksimum z sesji oraz zużyta energia (Wh).
- **Razem**: suma poboru mocy, jej średnia i maksimum, łączna energia,
  czas sesji i przycisk "Wyzeruj licznik". Maksimum sumy to największa
  chwilowa suma, a nie suma osobnych maksimów.
- Każdy kafelek (procesor, karta, Razem) ma wykres z ostatnich 120 s, zmianę
  z ostatnich 5 s i plakietkę obciążenia: NISKI poniżej 35%, ŚREDNI do 80%,
  WYSOKI od 80% limitu mocy (procesor: PL2 Intel lub PPT AMD, karta: domyślny
  limit sterownika).
- Pasek sesji: czas, zużyta energia, koszt (cena za kWh i waluta w OPTIONS,
  domyślnie 0,90 zł; 0 ukrywa koszt) i "Wyzeruj licznik".
- Licznik działa od uruchomienia programu, także przy zminimalizowanym
  oknie. Przerwy dłuższe niż 5 s (uśpienie komputera) nie są doliczane.
- To pobór samego procesora i karty graficznej według ich własnych
  czujników, a nie całego komputera; płyta, dyski, wentylatory i straty
  zasilacza nie są wliczone.
- Moc procesora wymaga uprawnień administratora i sterownika PawnIO.

## RGB - oświetlenie

Wszystkie obsługiwane podświetlenia w jednej zakładce (od 0.28.0; wcześniej
RAM i Skydimo były w OPTIONS - ustawienia przechodzą same). Dla każdego
urządzenia: **Bez zmian** (program nie dotyka podświetlenia albo oddaje
sterowanie urządzeniu), **Kolor** (jeden kolor z jasnością 0-100 % i
efektem) albo **Wyłączone**. Efekty wykonuje samo urządzenie, a lista
pokazuje tylko te, które ono ma: stały kolor, oddychanie, miganie, cykl
kolorów, tęcza (fala); do tego szybkość (wolno / średnio / szybko). Przy cyklu
kolorów i tęczy urządzenie używa własnych kolorów. Skydimo i karty NVIDIA -
tylko stały kolor. "To samo ustawienie na wszystkich urządzeniach" ustawia jeden
kolor wszędzie. "Szukaj urządzeń ponownie" po podłączeniu nowego sprzętu.

Program niczego nie zapisuje w pamięci urządzeń: po wyłączeniu zasilania
wracają do swoich efektów, a Tune Center ustawia kolor ponownie przy każdym
starcie (także z autostartem Windows). Wykrywanie to same odczyty; pierwszy
zapis następuje dopiero po wyborze trybu. Aplikacje producentów (G HUB,
Armoury Crate, iCUE, MSI Center, RGB Fusion, SkyDimo) i OpenRGB mogą
nadpisywać ustawienia - najlepiej, żeby nie działały jednocześnie.

| Urządzenia | Jak | Sprawdzone |
|---|---|---|
| Pamięć RAM z kontrolerem ENE (np. G.Skill Trident Z RGB) | SMBus przez moduł PawnIO `SmbusI801.bin` (Intel) albo `SmbusPIIX4.bin` (AMD); wymaga administratora | tak ("DIMM_LED-0103", Intel; magistrala AMD sprawdzona z pamięcią Corsair) |
| Pamięć RAM Corsair (Vengeance RGB PRO, Dominator Platinum RGB, DDR4) | SMBus jak wyżej, kontroler Corsair 0x58-0x5F | tak (Vengeance RGB PRO, płyta AMD B450) |
| Taśmy Skydimo (USB CH340 1A86:7523) | port szeregowy, protokół Adalight, kolor powtarzany co sekundę | tak (SK0127) |
| Logitech G - klawiatury, myszy, odbiorniki LIGHTSPEED | HID++ 2.0, funkcje 0x8071 / 0x8070 wykrywane w urządzeniu | tak (G915 TKL na kablu) |
| Karty NVIDIA ze strefami oświetlenia w sterowniku (np. Founders Edition) | publiczne NVAPI `ClientIllumZones*` | odczyt (MSI RTX 5070 nie ma stref) |
| ASUS Aura (płyty główne i kontrolery ARGB na USB) | raporty HID 0xEC | nie |
| Gigabyte RGB Fusion 2 (USB, ITE 8297/5702/5711) | raporty "feature" 0xCC | nie |
| Corsair Lighting Node Core / Pro, Commander Pro, LS100 | raporty HID, tryb stałego koloru | nie |
| MSI Mystic Light (płyty główne MSI) | raport "feature" 0x52 - odczyt, zmiana stref, zapis | nie |

- RAM: kontroler jest rozpoznawany wyłącznie odczytem (podpis w rejestrach
  0xA0-0xAF); zapis idzie tylko pod adresy 0x70-0x77, nigdy do pamięci SPD
  modułów (0x50-0x57). Adresy nie są przemapowywane - po włączeniu komputera
  wszystkie moduły odpowiadają razem pod 0x77. Kolor i tryb są sprawdzane
  odczytem. Efekt sprzed pierwszej zmiany jest zapamiętany i wraca po
  wybraniu "Bez zmian".
- Skydimo: aplikacja SkyDimo musi być zamknięta (zajmuje port). "Bez zmian"
  gasi taśmę i zwalnia port.
- Logitech, karta NVIDIA i MSI Mystic Light przywracają poprzedni efekt od
  razu po wybraniu "Bez zmian"; ASUS, Gigabyte i Corsair - po wyłączeniu
  zasilania.
- Urządzenia nietestowane są oznaczone w zakładce. Karty graficzne partnerów
  (MSI, ASUS, Gigabyte…) z własnym układem podświetlenia na magistrali I²C
  karty nie są obsługiwane - zapis w złe miejsce tej magistrali mógłby
  uszkodzić kartę.

## TUNE - opóźnienia i DPC

Ustawienia Windows, które wpływają na opóźnienie wejścia i czasy DPC. Każda
pozycja ma przełącznik pokazujący stan odczytany z systemu: ON (zielony) -
ustawiona wartość zalecana, OFF (czerwony) - wartość Windows, "n/d" - nie
dotyczy tego komputera. Kliknięcie przełącza; gałka przesuwa się dopiero po
zapisie i odczycie. Nad listą licznik włączonych pozycji. Przed pierwszą zmianą pozycji
oryginalna wartość trafia do `tune-backup.json` obok programu, a każdy zapis
jest sprawdzany odczytem. "Przywróć ustawienia Windows" cofa wszystko.
Oznaczenie RESTART: zmiana działa po ponownym uruchomieniu komputera.

Sprawdzone (Microsoft, producenci sprzętu):
- Memory Integrity i Virtual Machine Platform wyłączone (Microsoft: kilka %
  w grach; mniej ochrony jądra, bez WSL i maszyn wirtualnych).
- Karta sieciowa: Interrupt Moderation i Energy Efficient Ethernet wyłączone
  (karta restartuje się na kilka sekund).
- Karta sieciowa: oszczędzanie energii wyłączone - te ustawienia, które karta
  ma (Green Ethernet, Gigabit Lite, Advanced EEE, Power Saving Mode, Auto
  Disable Gigabit, Ultra Low Power Mode, Selective Suspend, wyłączanie karty
  przez Windows).
- Plan zasilania: USB Selective Suspend i PCI Express Link State Power
  Management wyłączone; plan Ultimate Performance (nie dla Ryzen X3D).
- Przyspieszanie wskaźnika myszy wyłączone (ruch liniowy, działa od razu).
- SysMain (Superfetch / Prefetch) wyłączony - tylko bez dysków HDD.
- Procesor: parkowanie rdzeni P i E wyłączone (bez wybudzania rdzeni przy
  nagłym obciążeniu; brakujący zakres ustawienia w rejestrze jest uzupełniany).
- NVIDIA (profil globalny): V-Sync włączona i limit klatek nieco poniżej
  odświeżania (dla G-SYNC), Low Latency Mode On.

Do przetestowania na własnym sprzęcie (porównaj LatencyMon przed i po):
- NVIDIA: tryb energii „maksymalna wydajność” (wyższy pobór w spoczynku).
- Tryb MSI przerwań dla karty graficznej, sieci, audio i USB.
- Kompresja pamięci wyłączona (przy 16 GB RAM i więcej).
- Fault Tolerant Heap wyłączony, szybkie uruchamianie wyłączone.
- `Win32PrioritySeparation` = 0x26, `SystemResponsiveness` = 10.
- Multi-Plane Overlay (MPO) wyłączone (`OverlayTestMode` = 5, jak w poprawce
  NVIDIA) - przy migotaniu i przycięciach w grach w oknie bez ramki i przy
  kilku monitorach; nowsze Windows 11 mogą to ignorować; działa po restarcie.

Celowo pominięte: `bcdedit useplatformclock/disabledynamictick` (sprzeczne
wyniki, problemy z myszą) i `NetworkThrottlingIndex` (więcej przerwań).
Większość pozycji wymaga uprawnień administratora.

## BENCH - test wydajności i stress test

- **Wydajność procesora** (w stylu CPU-Z): po 10 s na jednym wątku (rdzenie
  P - Windows wybiera najszybszy) i na wszystkich wątkach. Skala jest własna,
  wyniki nie są porównywalne z CPU-Z - służą do porównań na tym samym
  komputerze, np. przed i po undervoltingu. Historia 10 ostatnich wyników.
- **Stress test**: pełne obciążenie wszystkich wątków (AVX2 + FMA, gdy
  procesor je ma) przez 5-60 min albo do zatrzymania, wykresy temperatury i
  mocy procesora, automatyczne zatrzymanie przy 85/90/95 °C. Każdy wynik
  obliczeń jest sprawdzany - błąd oznacza niestabilne ustawienia procesora.
  Limit temperatury wymaga odczytu temperatury procesora (proces czujników,
  administrator).
- **Stabilność karty graficznej**: 2 minuty pełnego obciążenia przez
  Direct3D 11 - obliczenia FP32 na całym rdzeniu, przebieg mieszany
  (pamięć + obliczenia, największy pobór mocy) i zapis/odczyt 512 MB pamięci
  karty. Każdy wynik obliczeń i każdy odczyt pamięci jest sprawdzany - błędy
  oznaczają niestabilne podkręcenie albo undervolting. Po teście pokazuje się
  okno z wynikiem (GFLOPS FP32 i GB/s pamięci), oceną STABILNA/NIESTABILNA,
  porównaniem z najlepszym wynikiem oraz średnią/min./maks. i wykresami z
  2 minut: zegar rdzenia, zegar pamięci, temperatura, pobór mocy. Wyniki
  pełnych testów trafiają do historii.

OPTIONS i ABOUT otwierają się przyciskami w prawym dolnym rogu, obok
UPDATE.

## Minimalizowanie do zasobnika

Domyślnie przycisk minimalizacji chowa okno do zasobnika systemowego (ikona
przy zegarze) zamiast na pasek zadań. Lewy klik na ikonie przywraca okno,
prawy otwiera menu "Pokaż Tune Center" / "Zakończ". Sterowanie wentylatorami
i licznik energii działają dalej. Pole "Minimalizuj do zasobnika" w zakładce OPTIONS
wyłącza to zachowanie. Autostart uruchamia program od razu w zasobniku.

Program działa w jednej kopii: ponowne uruchomienie pokazuje okno już
działającego programu (także schowane w zasobniku). Jeśli powłoka Windows nie
ma obszaru powiadomień (np. zastępcza powłoka zamiast Eksploratora - brak okna
`Shell_TrayWnd`), okno nie jest chowane, tylko zwyczajnie minimalizowane na
pasek zadań; OPTIONS pokazuje wtedy notatkę.

Proces czujników `sensor-bridge.exe` jest uruchamiany ponownie, gdy sam się
zakończy (np. po awarii biblioteki sterownika), do 5 razy z rzędu; Windows nie
pokazuje wtedy okna "program przestał działać", a wentylatory dostają swoje
ustawienia od nowa.

Okno schowane w zasobniku nie jest rysowane i praktycznie nie obciąża
procesora (wcześniejsze wersje z eframe 0.29 zajmowały wtedy cały rdzeń).
Animacja wentylatorów w zakładce FAN jest ograniczona do 20 klatek/s.

Windows może schować nową ikonę pod strzałką "^" przy zegarze; można ją
stamtąd przeciągnąć na pasek.

## Zmiana rozmiaru okna

Układ dopasowuje się do okna: zakładki, przyciski i pola w nagłówku
zawijają się do kolejnych wierszy, kafelki w "Power" i "Czujniki" układają
się w tylu kolumnach, ile się mieści, a dłuższe listy można przewijać.
Najmniejszy rozmiar okna to 420 x 420.

## Język

Program ma dwa języki: polski i angielski. Przełącza się je w zakładce OPTIONS
("Język / Language"); zmiana działa od razu, tylko menu ikony w zasobniku
zmienia język po ponownym uruchomieniu. Nazwy zakładek są takie same w obu
językach.

Instalator pyta o język na początku. Wybór trafia do pliku
`default-language.txt` obok programu i jest językiem domyślnym, dopóki nie
zmienisz go w OPTIONS (własny wybór jest zapisany w ustawieniach i ma
pierwszeństwo, także po aktualizacji).

## Czcionka

Przyciski zakładek (GPU, CPU, FAN...) używają czcionki Audiowide wbudowanej w
program. Cała pozostała treść jest pisana domyślną czcionką interfejsu.

## Aktualizacja sterownika NVIDIA

Program sprawdza na serwerze NVIDIA najnowszy sterownik Game Ready (WHQL)
dla Twojej karty - 30 s po starcie i potem co 6 godzin (OPTIONS: "Sprawdzaj
aktualizacje sterownika NVIDIA", "Sprawdź teraz"). Gdy jest nowszy niż
zainstalowany, na dole okna pojawia się "Nowy sterownik NVIDIA X" i przycisk
"Zainstaluj X": program pobiera paczkę z download.nvidia.com, rozpakowuje
tylko sterownik grafiki, audio HDMI i PhysX (bez NVIDIA App i telemetrii) i
instaluje je w trybie cichym, bez restartu. Ekran mignie kilka razy - zamknij
wcześniej gry.

## Aktualizacje

Przycisk UPDATE (sprawdzenie aktualizacji) jest w prawym dolnym rogu okna. Program pyta
GitHub o najnowsze wydanie repozytorium `Atimaniac/tune-center` i porównuje
jego znacznik z własną wersją. Gdy jest nowsza, w tym samym miejscu pojawia
się przycisk "Zainstaluj X" i odnośnik "co nowego" do strony wydania.

"Zainstaluj X":

- pobiera załącznik `*Setup.exe` wydania do katalogu tymczasowego Windows
  (`TuneCenter-Setup-update.exe`),
- sprawdza, że to plik .exe, uruchamia go w trybie cichym
  (`/SILENT /SUPPRESSMSGBOXES /NORESTART /LANG=pl|en`, język jak w programie)
  i zamyka program zwykłą drogą - wentylatory i karta wracają do automatu,
- instalator podmienia pliki, zostawia ustawienia i profile, a na końcu
  uruchamia program ponownie.

Pobierane są tylko pliki spod adresu
`https://github.com/Atimaniac/tune-center/releases/download/...` (także
przekierowania tylko po HTTPS). Gdy wydanie nie ma takiego instalatora,
zamiast przycisku jest odnośnik "Dostępna wersja X - pobierz" do strony
wydania. Gdy pobieranie się nie uda, pojawia się błąd i odnośnik
"pobierz ręcznie".

Sprawdzanie odbywa się też przy każdym starcie (można to wyłączyć w OPTIONS);
wtedy informacja pojawia się tylko wtedy, gdy jest nowsza wersja.

Żeby to działało, wydania trzeba publikować na GitHubie:

1. Podnieś wersję w `Cargo.toml` i `installer.iss`, zbuduj instalator.
2. Na stronie repozytorium: Releases > Draft a new release.
3. Znacznik (tag) w postaci `v0.17.0` - taki sam numer jak w `Cargo.toml`.
4. Dołącz `TuneCenter-Setup.exe` (nazwa musi kończyć się na `Setup.exe`)
   i opublikuj (nie jako pre-release).

Instalator można też uruchomić cicho samodzielnie:
`TuneCenter-Setup.exe /SILENT /SUPPRESSMSGBOXES /LANG=pl`. Pytania dostają
wtedy domyślne odpowiedzi: aktualizacja i naprawa przechodzą, zastąpienie
nowszej wersji starszą jest odrzucane.

Repozytorium musi być publiczne. Zapytanie wykonuje `curl.exe` dołączony do
Windows 10 i 11. Nazwę repozytorium ustawia stała `REPO` w `src/update.rs`.

## Motyw

Lista "Motyw" w zakładce OPTIONS: **Tune** (granat z niebieską poświatą),
**Neon Green** (czerń i zieleń, ukośne smugi światła), **Neon City** (fiolet,
cyjan i magenta, miasto nocą i postać w nagłówku, ulica z odbiciami neonów na
dole okna) i **Crimson** (czerń i czerwień). Pole "Jasny motyw" (białe tło,
czarny tekst) działa tylko w motywie Tune. Układ i działanie programu są we
wszystkich motywach takie same. Wybór jest zapamiętywany.

## Ikona i nazwa

- Ikona programu jest w `assets\` (`icon.ico` dla pliku .exe i instalatora,
  `icon-128.rgba` dla okna, `icon.png` do podglądu).
- Po zmianie nazwy z PCTune program sam przejmuje stare ustawienia
  (`pctune-config.json`) i stare zadanie autostartu.

## Skąd pochodzą odczyty

| Odczyt | Źródło | Sterownik PawnIO |
| --- | --- | --- |
| Karta Nvidia: temperatura, hotspot, zegary, napięcie, użycie | NVAPI (sterownik Nvidii) | nie |
| Karta Nvidia: pobór mocy, wysterowanie wentylatora | NVML (sterownik Nvidii) | nie |
| Karta AMD: temperatura, hotspot, wentylator, moc, zegary | ADL (sterownik AMD) | nie |
| Temperatura karty w SENSORS | wprost ze sterownika karty | nie |
| Moc karty w POWER | wprost ze sterownika karty; czujniki są zapasem | nie |
| Wentylatory karty w FAN (odczyt i sterowanie) | wprost ze sterownika karty (NVAPI / ADL) | nie |
| Dyski | API dysków Windows; czujniki są zapasem | nie |
| Temperatura i moc procesora | LibreHardwareMonitor | tak |
| Wentylatory i temperatury płyty głównej | LibreHardwareMonitor | tak |
| Zakładka CPU (strojenie) | moduły PawnIO | tak |
| Podświetlenie RAM (zakładka RGB) | moduł PawnIO SmbusI801 | tak |

PawnIO jest opcjonalny: instalator pozwala go odznaczyć, a program bez niego
pokazuje wszystko z górnej części tabeli. Licznik energii w POWER działa wtedy
z samej karty graficznej - także gdy proces czujników w ogóle nie wystartował.
Gdy okno jest schowane w zasobniku, interfejs nie jest rysowany, ale odczyty
ze sterownika karty są dalej odświeżane, gdy proces czujników przysyła dane
(co sekundę). Jeśli odczyt ze sterownika jest starszy niż 3 s, licznik
przechodzi na czujniki.

### Wentylatory karty graficznej przez sterownik

Program steruje wentylatorami karty sam, w osobnym wątku (działa też przy
oknie schowanym w zasobniku): Nvidia przez NVAPI (ClientFanCoolers), AMD
przez ADL (Overdrive 5). Wszystkie wentylatory jednej karty dostają to samo
wysterowanie i są w zakładce FAN jedną kartą "GPU Fan".

Zabezpieczenia:

- wentylator w trybie Auto nie jest dotykany - steruje nim karta,
- gdy karta osiągnie 88 °C, program oddaje wentylator automatowi karty,
  niezależnie od ustawień,
- krzywa bez odczytu temperatury też oddaje sterowanie karcie,
- przy zamykaniu programu karta wraca do automatu.

Jeśli program zostanie zabity (np. z Menedżera zadań) w trakcie sterowania
ręcznego, wentylator zostaje na ostatnim wysterowaniu do ponownego
uruchomienia programu albo restartu komputera.

Gdy sterownik karty nie udostępnia sterowania, program sam wraca do
sterowania przez proces czujników (LibreHardwareMonitor). Można to też
wymusić w OPTIONS, wyłączając "Wentylatory karty graficznej przez sterownik
karty". Po zmianie sposobu sterowania ustawienia wentylatora karty (tryb,
krzywa) trzeba wybrać ponownie, bo wentylator ma inny identyfikator.

### Temperatury dysków

Zakładka SENSORS czyta temperatury dysków wprost z Windows
(`IOCTL_STORAGE_QUERY_PROPERTY`), co 10 sekund. Tą drogą temperaturę podają
przede wszystkim dyski NVMe; dla dysków, których Windows nie podaje (część
SATA), zostaje odczyt z procesu czujników.

Starsze wersje LibreHardwareMonitor (do 0.9.4) zamiast PawnIO używały
sterownika WinRing0. Nie wracamy do niego: Windows Defender blokuje go jako
podatny na ataki.

Odczyty karty graficznej działają także wtedy, gdy PawnIO nie jest
zainstalowany albo proces czujników nie wystartował.

Temperatury procesora i wentylatorów płyty głównej nie da się odczytać bez
sterownika jądra. Intel i AMD nie udostępniają do tego API dla zwykłych
programów: temperatura procesora jest w rejestrach MSR, a wentylatorami
steruje układ Super I/O na płycie - do obu Windows dopuszcza tylko sterownik.
PawnIO jest takim sterownikiem (podpisanym); alternatywą byłby inny sterownik
tego samego rodzaju, a nie jego brak.

## Informacje o karcie: hotspot, HAGS, Resizable BAR

W zakładce NVIDIA > Overclock > Informacje:

- **Temperatura hotspot** - najcieplejszy punkt karty, odczytywany wprost ze
  sterownika (NVAPI / ADL). Karty GeForce RTX serii 50 nie udostępniają go w
  API sterownika (sprawdzone pod obciążeniem na RTX 5070: wszystkie czujniki
  NVAPI i NVML to rdzeń albo pamięć) - wiersz Hot Spot pokazuje wtedy
  "niedostępny". GPU-Z odczytuje go własnym sterownikiem jądra. "Temperatura
  GPU" na RTX 50 ma dokładność do części stopnia.
- **HAGS** (sprzętowo przyspieszane planowanie GPU) - stan ustawienia Windows
  i przycisk Włącz/Wyłącz. Program zmienia tę samą wartość co przełącznik w
  Ustawieniach Windows (`HwSchMode` w rejestrze). Zmiana działa po restarcie
  komputera.
- **Resizable BAR** - tylko odczyt. Program sprawdza rozmiar okna pamięci
  przydzielonego karcie: 256 MB oznacza wyłączony, większe okno - włączony.
  Rozmiar pochodzi z zasobów przydzielonych karcie przez Windows (CfgMgr32,
  jak karta "Zasoby" w Menedżerze urządzeń, z dużymi oknami 64-bitowymi),
  odczytywanych raz przy starcie, w tle; gdy się nie uda, w wierszu jest
  "nieznane".
  Resizable BAR włącza się w BIOS-ie płyty głównej.

## Diagnostyka zgodności

OPTIONS > "Diagnostyka zgodności" (albo `tunecenter.exe --diagnose` bez okna)
sprawdza, co na danym komputerze działa, a co nie - **wyłącznie odczyty**,
nic nie jest zmieniane. Raport `tunecenter-diagnostics.txt` (obok programu,
a gdy tam nie wolno pisać - w %TEMP%) zawiera dla każdego punktu OK / WARN /
FAIL i surowe dane:

- system: Windows, administrator, UAC, płyta i BIOS, .NET 11, pliki procesu
  czujników, moduły i sterownik PawnIO,
- procesor: identyfikator CPUID, surowe odczyty MSR (Intel) albo generacja,
  wersja tabeli PM i odczyty SMU (AMD), wszystko, co pokazuje zakładka CPU,
- karta: identyfikatory PCI, okna pamięci (ReBAR), brakujące funkcje NVAPI /
  NVML, ADL, odczyty czujników, wentylator, offsety, krzywa V/F,
- czujniki (LibreHardwareMonitor): układ Super I/O, temperatury, wentylatory,
- dyski, RGB (wykryte urządzenia i lista wszystkich urządzeń USB HID), TUNE,
  gry i silniki do kompilacji shaderów, raporty awarii.

Bez numerów seryjnych i adresów MAC; nazwa użytkownika i komputera są
zastępowane. "Zgłoś na GitHubie" kopiuje raport do schowka i otwiera nowe
zgłoszenie z szablonu "Compatibility report" (tytuł i podsumowanie są
wypełnione, raport wkleja się Ctrl+V). Uruchomiona z OPTIONS korzysta z
działającego procesu czujników; w trybie `--diagnose` uruchamia go na chwilę
tylko wtedy, gdy nie działa już inny (żeby nie oddać wentylatorów BIOS-owi).

## Awarie

Gdy program albo proces czujników ulegnie awarii, pokazuje się okno z opisem
błędu (kod, moduł, w którym wystąpił, komunikat .NET procesu czujników).
Raport trafia do pliku `tunecenter-crash.log` albo `sensor-bridge-crash.log`
obok programu; przy następnym starcie program pokazuje ostatni raport.
Proces czujników po awarii uruchamia się ponownie sam.

## Znane ograniczenia

- Zakładka POWER pokazuje tylko procesor i kartę graficzną. Dyski i płyta
  główna nie mają czujników poboru mocy, więc nie ma czego odczytać.

- Mnożniki rdzeni E (rejestr 0x650) wymagają modułu IntelMSR.bin z
  PawnIO.Modules 0.2.12 lub nowszego; starszy moduł odrzuca ten rejestr
  (błąd 0x80070005) - program pokazuje wtedy krótką informację zamiast błędu.

## Bezpieczeństwo

Wentylatory:

- Dolna granica wysterowania to 20 % (stała `MIN_PCT` w `src/config.rs`).
- Po zamknięciu programu wszystkie wentylatory wracają pod sterowanie BIOS-u.
- Gdy czujnik temperatury krzywej przestanie odpowiadać, wentylator wraca
  do trybu BIOS-u.

Karta graficzna:

- Ustawienia nie są zapisywane w karcie. Znikają po restarcie systemu lub
  sterownika, więc zawieszenie po zbyt agresywnym OC naprawia restart.
- Zmieniaj po 10-25 MHz i testuj stabilność po każdej zmianie.

Nie uruchamiaj równocześnie innych programów sterujących tym samym sprzętem
(MSI Afterburner, FanControl, oprogramowanie producenta płyty) - będą się
nadpisywać.

## Prywatność

Tune Center nie ma telemetrii, kont ani reklam. Łączy się z GitHubem
(aktualizacje programu) i z serwerami NVIDIA (aktualizacje sterownika)
wyłącznie po to, żeby sprawdzić i pobrać aktualizację; oba sprawdzania można
wyłączyć. Szczegóły: [PRIVACY.pl.md](PRIVACY.pl.md).

## Licencja

Tune Center - strojenie i monitorowanie komputera w jednym oknie\
Copyright (C) 2026 Atimaniac

Ten program jest wolnym oprogramowaniem: możesz go rozpowszechniać i
modyfikować na warunkach Powszechnej Licencji Publicznej GNU (GNU General
Public License), wydanej przez Free Software Foundation, w wersji 3 albo
(według Twojego uznania) dowolnej nowszej. Identyfikator SPDX:
`GPL-3.0-or-later`.

Program jest rozpowszechniany z nadzieją, że będzie użyteczny, ale BEZ
ŻADNEJ GWARANCJI, nawet domniemanej gwarancji PRZYDATNOŚCI HANDLOWEJ albo
PRZYDATNOŚCI DO OKREŚLONEGO CELU. Szczegóły są w Powszechnej Licencji
Publicznej GNU.

Kopia licencji jest w pliku [LICENSE](LICENSE) (wiążący jest tekst
angielski); znajdziesz ją też pod adresem <https://www.gnu.org/licenses/>.

Co to oznacza w praktyce:

- Możesz używać programu w dowolnym celu, czytać i zmieniać jego kod źródłowy
  oraz przekazywać kopie - za darmo albo odpłatnie.
- Jeśli rozpowszechniasz program albo jego zmienioną wersję, musisz robić to
  na tej samej licencji (GPL-3.0-or-later), zachować noty o prawach
  autorskich i udostępnić pełny kod źródłowy.
- Program nie ma gwarancji, a autor nie odpowiada za szkody wynikłe z jego
  używania.

Plik LICENSE jest instalowany obok programu, a nota licencyjna jest widoczna
w zakładce ABOUT. Składniki innych autorów zachowują swoje licencje - zobacz
[THIRD-PARTY.pl.md](THIRD-PARTY.pl.md).

## Stan projektu

- Projekt nie był kompilowany w całości ani uruchamiany na prawdziwym
  sprzęcie; możliwe drobne błędy kompilacji do poprawienia. Sprawdzona jest
  składnia wszystkich plików Rust i obliczanie krzywej wentylatorów.
- Obsługa kart AMD (`src/gpu/amd.rs`) opiera się na oficjalnym ADL SDK
  (struktury, identyfikatory i kolejność wywołań z przykładu Overdrive8).
  Typy i rozmiary struktur są sprawdzone kompilatorem, ale kod nie był
  uruchomiony na karcie Radeon.
- Funkcje krzywej V/F i zapisu offsetów są nieudokumentowane przez Nvidię.
  Jeśli sterownik odrzuci strukturę, program pokaże błąd w pasku stanu
  (np. "niezgodna wersja struktury") i niczego nie zapisze.
- W BIOS-ie złącza wentylatorów powinny być w trybie PWM lub DC zgodnym
  z podłączonym wentylatorem; inaczej zmiana wysterowania nic nie da.
- Nazwy typu "Fan #2" pochodzą z układu Super I/O. Które złącze jest które,
  najłatwiej sprawdzić, ustawiając po kolei 100 % w trybie ręcznym.
