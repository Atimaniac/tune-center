; Instalator Tune Center (Inno Setup 6.3 lub nowszy).
; Budowanie: .\build-installer.ps1  ->  installer-output\TuneCenter-Setup.exe
;
; Instalator kopiuje zawartosc dist\ i - za zgoda uzytkownika - instaluje
; brakujace skladniki przez winget: .NET 11 Runtime (zawsze, takze przy cichej
; aktualizacji z programu - proces czujnikow od 0.25.0 wymaga .NET 11) oraz,
; za zgoda uzytkownika, sterownik PawnIO.
;
; Gdy program jest juz zainstalowany, instalator dziala jako aktualizacja:
; informuje o wersjach, zamyka dzialajacy program, podmienia pliki w tym samym
; katalogu i zostawia ustawienia. AppGuid musi pozostac ten sam we wszystkich
; wersjach, a AppVersion trzeba podnosic razem z wersja w Cargo.toml.
;
; Instalator jest dwujezyczny (polski i angielski). Wybrany jezyk trafia do
; pliku default-language.txt obok programu i jest domyslnym jezykiem programu,
; dopoki uzytkownik nie zmieni go w zakladce OPTIONS.
;
; Tryb cichy (aktualizacja z programu): /SILENT /SUPPRESSMSGBOXES /NORESTART /LANG=pl|en
; Pytania maja wtedy domyslne odpowiedzi (SuppressibleMsgBox), a po cichej
; aktualizacji program jest uruchamiany ponownie.

#define AppName "Tune Center"
#define AppVersion "0.32.0"
#define AppGuid "2F9C1B7A-4D3E-4E8B-B1C6-7A5D0E9F3C42"
#define AppExe "tunecenter.exe"

[Setup]
AppId={{{#AppGuid}}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=Atimaniac
AppPublisherURL=https://github.com/Atimaniac/tune-center
AppSupportURL=https://github.com/Atimaniac/tune-center/issues
AppUpdatesURL=https://github.com/Atimaniac/tune-center/releases
AppCopyright=Copyright (C) 2026 Atimaniac
VersionInfoCompany=Atimaniac
VersionInfoProductName={#AppName}
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#AppExe}
OutputDir=installer-output
OutputBaseFilename=TuneCenter-Setup
SetupIconFile=assets\icon.ico
WizardImageFile=assets\wizard-large.bmp
WizardSmallImageFile=assets\wizard-small.bmp
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Dzialajacy program zamyka PrepareToInstall w sekcji [Code].
CloseApplications=no
UsePreviousAppDir=yes
DisableDirPage=auto
ShowLanguageDialog=yes

[Languages]
Name: "pl"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
pl.TaskPawnIO=Zainstaluj sterownik PawnIO (opcjonalny: temperatura i moc procesora, wentylatory plyty glownej, zakladka CPU)
en.TaskPawnIO=Install the PawnIO driver (optional: CPU temperature and power, motherboard fans, CPU tab)
pl.GroupMissing=Brakujace skladniki (pobierane z internetu):
en.GroupMissing=Missing components (downloaded from the internet):
pl.TaskDesktop=Utworz skrot na pulpicie
en.TaskDesktop=Create a desktop shortcut
pl.GroupShortcuts=Skroty:
en.GroupShortcuts=Shortcuts:
pl.StatusDotNet=Instalowanie .NET 11 Runtime (wymagany do odczytu czujnikow)...
en.StatusDotNet=Installing .NET 11 Runtime (required for sensor readings)...
pl.StatusPawnIO=Instalowanie sterownika PawnIO...
en.StatusPawnIO=Installing the PawnIO driver...
pl.RunApp=Uruchom {#AppName}
en.RunApp=Launch {#AppName}
pl.UpdateAvailable=Dostepna jest aktualizacja {#AppName}.%n%nZainstalowana wersja: %1%nNowa wersja: {#AppVersion}%n%nUstawienia i profile zostana zachowane. Zaktualizowac teraz?
en.UpdateAvailable=An update for {#AppName} is available.%n%nInstalled version: %1%nNew version: {#AppVersion}%n%nSettings and profiles will be kept. Update now?
pl.SameVersion={#AppName} {#AppVersion} jest juz zainstalowany.%n%nZainstalowac te sama wersje ponownie (naprawa plikow)?
en.SameVersion={#AppName} {#AppVersion} is already installed.%n%nInstall the same version again (repair files)?
pl.NewerInstalled=Zainstalowana jest nowsza wersja {#AppName} (%1).%n%nTen instalator zawiera starsza wersje {#AppVersion}. Czy na pewno zastapic nia nowsza?
en.NewerInstalled=A newer version of {#AppName} (%1) is installed.%n%nThis installer contains the older version {#AppVersion}. Replace the newer one anyway?
pl.UpdateButton=Aktualizuj
en.UpdateButton=Update
pl.ReadyTitle=Gotowy do aktualizacji
en.ReadyTitle=Ready to update
pl.ReadyDescription={#AppName} %1 zostanie zaktualizowany do wersji {#AppVersion}.
en.ReadyDescription={#AppName} %1 will be updated to version {#AppVersion}.
pl.ReadyText=Kliknij Aktualizuj, aby kontynuowac. Dzialajacy program zostanie zamkniety, a ustawienia i profile pozostana bez zmian.
en.ReadyText=Click Update to continue. The running program will be closed; settings and profiles stay unchanged.
pl.UpdatingTitle=Aktualizowanie
en.UpdatingTitle=Updating
pl.UpdatingDescription=Poczekaj, az instalator zaktualizuje {#AppName} na tym komputerze.
en.UpdatingDescription=Please wait while Setup updates {#AppName} on this computer.
pl.FinishedTitle=Aktualizacja zakonczona
en.FinishedTitle=Update complete
pl.FinishedText={#AppName} zostal zaktualizowany do wersji {#AppVersion}.
en.FinishedText={#AppName} has been updated to version {#AppVersion}.
pl.MissingComponents=Nie wykryto ponizszych skladnikow. Bez .NET 11 nie dzialaja czujniki i sterowanie wentylatorami. Bez PawnIO program dziala, ale nie pokazuje temperatury i mocy procesora, nie steruje wentylatorami plyty glownej i nie ma zakladki CPU. Mozna je zainstalowac recznie:
en.MissingComponents=The components below were not detected. Without .NET 11, sensors and fan control do not work. Without PawnIO the program works, but shows no CPU temperature or power, cannot control motherboard fans and has no CPU tab. You can install them manually:

[Tasks]
Name: "pawnio"; Description: "{cm:TaskPawnIO}"; GroupDescription: "{cm:GroupMissing}"; Check: NeedsPawnIO
Name: "desktopicon"; Description: "{cm:TaskDesktop}"; GroupDescription: "{cm:GroupShortcuts}"; Flags: unchecked

[Files]
; Wlasnych ustawien i plikow debugowania nie pakujemy do instalatora.
Source: "dist\*"; DestDir: "{app}"; Excludes: "*.pdb,*crash*.log,tunecenter-config.json,pctune-config.json,gpu-profile.json,gpu-profiles.json,gpu-settings.json,gpu-autoapply.pending,default-language.txt"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Program startuje bez uprawnien administratora (od 0.17.6). Usuwa wpis
; "zawsze jako administrator" dodawany przez starsze wersje instalatora.
Root: HKLM; Subkey: "SOFTWARE\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"; ValueType: none; ValueName: "{app}\{#AppExe}"; Flags: deletevalue uninsdeletevalue

[Run]
; .NET 11 Runtime instalowany zawsze, gdy go brak (bez zadania do odznaczenia -
; przy aktualizacji Inno Setup pamieta wybor zadan z poprzedniej instalacji).
Filename: "{cmd}"; Parameters: "/C winget install --id Microsoft.DotNet.Runtime.11 --exact --silent --accept-package-agreements --accept-source-agreements"; StatusMsg: "{cm:StatusDotNet}"; Check: NeedsDotNet; Flags: waituntilterminated runhidden
Filename: "{cmd}"; Parameters: "/C winget install --id namazso.PawnIO --exact --silent --accept-package-agreements --accept-source-agreements"; StatusMsg: "{cm:StatusPawnIO}"; Tasks: pawnio; Flags: waituntilterminated
Filename: "{app}\{#AppExe}"; Description: "{cm:RunApp}"; Flags: nowait postinstall skipifsilent
; Po cichej aktualizacji (przycisk "Zainstaluj" w programie) program wraca sam.
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: SilentUpgrade

[InstallDelete]
; Pozostalosc po procesie czujnikow na .NET 8 (w .NET 11 ta biblioteka jest wbudowana).
Type: files; Name: "{app}\System.Threading.AccessControl.dll"

[UninstallRun]
; Usuwa zadanie autostartu utworzone przez opcje "Uruchamiaj z systemem".
Filename: "schtasks"; Parameters: "/Delete /TN TuneCenter /F"; Flags: runhidden; RunOnceId: "RemoveAutostart"

[UninstallDelete]
Type: files; Name: "{app}\tunecenter-config.json"
Type: files; Name: "{app}\gpu-profile.json"
Type: files; Name: "{app}\gpu-profiles.json"
Type: files; Name: "{app}\gpu-settings.json"
Type: files; Name: "{app}\gpu-autoapply.pending"
Type: files; Name: "{app}\default-language.txt"

[Code]
const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{{#AppGuid}}_is1';

var
  { Wersja juz zainstalowanego programu; pusta przy pierwszej instalacji. }
  InstalledVersion: String;

function IsUpgrade: Boolean;
begin
  Result := InstalledVersion <> '';
end;

{ Cicha aktualizacja istniejacej instalacji - wtedy program jest uruchamiany ponownie. }
function SilentUpgrade: Boolean;
begin
  Result := WizardSilent and IsUpgrade;
end;

function ReadInstalledVersion: String;
begin
  Result := '';
  if not RegQueryStringValue(HKLM, UninstallKey, 'DisplayVersion', Result) then
    RegQueryStringValue(HKCU, UninstallKey, 'DisplayVersion', Result);
end;

{ Zwraca -1, 0 albo 1: zainstalowana wersja starsza, taka sama, nowsza od tej z instalatora. }
function CompareWithInstalled: Integer;
var
  Installed, This: Int64;
begin
  if StrToVersion(InstalledVersion, Installed) and StrToVersion('{#AppVersion}', This) then
    Result := ComparePackedVersion(Installed, This)
  else
    Result := CompareText(InstalledVersion, '{#AppVersion}');
end;

function InitializeSetup: Boolean;
var
  Order: Integer;
begin
  Result := True;
  InstalledVersion := ReadInstalledVersion;
  if not IsUpgrade then
    Exit;

  { Przy /SUPPRESSMSGBOXES obowiazuje ostatni argument: aktualizacja i naprawa
    przechodza, a zastapienie nowszej wersji starsza jest odrzucane. }
  Order := CompareWithInstalled;
  if Order < 0 then
    Result := SuppressibleMsgBox(FmtMessage(CustomMessage('UpdateAvailable'), [InstalledVersion]),
      mbConfirmation, MB_YESNO, IDYES) = IDYES
  else if Order = 0 then
    Result := SuppressibleMsgBox(CustomMessage('SameVersion'),
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDYES) = IDYES
  else
    Result := SuppressibleMsgBox(FmtMessage(CustomMessage('NewerInstalled'), [InstalledVersion]),
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES;
end;

{ Przy aktualizacji kreator mowi o aktualizowaniu, a nie o instalowaniu. }
procedure CurPageChanged(CurPageID: Integer);
begin
  if not IsUpgrade then
    Exit;

  if CurPageID = wpReady then
  begin
    WizardForm.NextButton.Caption := CustomMessage('UpdateButton');
    WizardForm.PageNameLabel.Caption := CustomMessage('ReadyTitle');
    WizardForm.PageDescriptionLabel.Caption :=
      FmtMessage(CustomMessage('ReadyDescription'), [InstalledVersion]);
    WizardForm.ReadyLabel.Caption := CustomMessage('ReadyText');
  end
  else if CurPageID = wpInstalling then
  begin
    WizardForm.PageNameLabel.Caption := CustomMessage('UpdatingTitle');
    WizardForm.PageDescriptionLabel.Caption :=
      CustomMessage('UpdatingDescription');
  end
  else if CurPageID = wpFinished then
  begin
    WizardForm.FinishedHeadingLabel.Caption := CustomMessage('FinishedTitle');
    WizardForm.FinishedLabel.Caption :=
      CustomMessage('FinishedText');
  end;
end;

{ Zamyka dzialajacy program (takze schowany w zasobniku), zeby mozna bylo
  podmienic pliki. Po zamknieciu glownego programu proces czujnikow sam
  oddaje wentylatory pod sterowanie BIOS-u i konczy prace; czekamy na to,
  a dopiero potem zamykamy go na sile, gdyby wciaz dzialal. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  TaskKill: String;
begin
  Result := '';
  TaskKill := ExpandConstant('{sys}\taskkill.exe');
  { Najpierw prosba o zamkniecie (WM_CLOSE): program oddaje wentylatory karty
    automatowi. Przy aktualizacji z programu jest juz zwykle zamkniety. }
  Exec(TaskKill, '/IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  if Code = 0 then
    Sleep(3000);
  Exec(TaskKill, '/IM {#AppExe} /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(2500);
  Exec(TaskKill, '/IM sensor-bridge.exe /F', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
end;

function NeedsDotNet: Boolean;
var
  FindRec: TFindRec;
begin
  Result := True;
  if FindFirst(ExpandConstant('{commonpf64}\dotnet\shared\Microsoft.NETCore.App\11.*'), FindRec) then
  begin
    Result := False;
    FindClose(FindRec);
  end;
end;

function NeedsPawnIO: Boolean;
begin
  Result := not (RegKeyExists(HKLM, 'SYSTEM\CurrentControlSet\Services\PawnIO')
    or DirExists(ExpandConstant('{commonpf64}\PawnIO')));
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Missing: String;
begin
  if CurStep <> ssPostInstall then
    Exit;

  { Jezyk wybrany w instalatorze jest domyslnym jezykiem programu. }
  SaveStringToFile(ExpandConstant('{app}\default-language.txt'), ActiveLanguage, False);

  { Jesli winget nie zadzialal albo uzytkownik odznaczyl skladnik, mowimy o tym wprost. }
  Missing := '';
  if NeedsDotNet then
    Missing := Missing + '- .NET 11 Runtime: https://dotnet.microsoft.com/download/dotnet/11.0' + #13#10;
  if NeedsPawnIO then
    Missing := Missing + '- PawnIO: https://pawnio.eu' + #13#10;

  if Missing <> '' then
    SuppressibleMsgBox(CustomMessage('MissingComponents') + #13#10 + #13#10 + Missing,
      mbInformation, MB_OK, IDOK);
end;
