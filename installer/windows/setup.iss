; FluxDown Windows Installer Script (Inno Setup) — GPUI desktop client
; This script is used by GitHub Actions to build the installer.
;
; Layout: fluxdown-desktop.exe (UI) + fluxdown-agent.exe (resident gateway / tray)
; + fluxdownd.exe (download daemon) + fluxdown_nmh.exe (browser relay) + MSVC CRT,
; all in {app}. AppId / install dir / output name are unchanged from the Flutter
; client, so the Flutter auto-updater (`setup.exe /SILENT`) upgrades in place.
; The GPUI in-app updater (native/agent/src/update/install) runs this installer with
; `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-` after the agent and daemon have exited;
; the silent [Run] entry then relaunches the desktop with `--after-update`.

#define MyAppName "FluxDown"
#define MyAppPublisher "FluxDown"
#define MyAppURL "https://fluxdown.com"
#define MyAppExeName "fluxdown-desktop.exe"
#define MyAgentExeName "fluxdown-agent.exe"
; Former Flutter executable, removed on upgrade
#define LegacyExeName "flux_down.exe"

; Version is passed from CI via /DMyAppVersion=x.y.z
#ifndef MyAppVersion
  #define MyAppVersion "1.0.0"
#endif

; Architecture is passed from CI via /DMyAppArch=x64 or /DMyAppArch=arm64
#ifndef MyAppArch
  #define MyAppArch "x64"
#endif

; Staged GPUI binaries, passed from CI via /DMySourceDir=<abs path>
#ifndef MySourceDir
  #define MySourceDir "..\..\build\gpui\windows-" + MyAppArch
#endif

[Setup]
AppId={{B7E3F2A1-5C4D-4E8F-9A6B-1D2E3F4A5B6C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=..\..\build\installer
OutputBaseFilename=FluxDown-{#MyAppVersion}-windows-{#MyAppArch}-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
#if MyAppArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
; 始终每用户安装：{autopf} → %LOCALAPPDATA%\Programs\FluxDown，从不请求 UAC。
; 运行期集成（NMH、URL 协议、.torrent 关联、自启、通知 AUMID）全部写 HKCU 与
; %LOCALAPPDATA%\FluxDown，不需要管理员权限。不开放「为所有用户安装」覆盖：
; 一旦选过，Inno 的 UsePreviousPrivileges 会让之后每次安装 / 静默自动更新都要求提权。
; 此前留下的全体用户安装由 [Code] RemoveLegacyAllUsersInstall 一次性迁移。
PrivilegesRequired=lowest
CloseApplications=force
SetupIconFile=..\..\assets\logo\windows\app_icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[CustomMessages]
english.OtherTasks=Other:
chinesesimplified.OtherTasks=其他：
english.FileAssociations=File associations:
chinesesimplified.FileAssociations=文件关联：
english.LaunchOnStartup=Launch at system startup
chinesesimplified.LaunchOnStartup=开机时自动启动
english.TorrentAssoc=Associate .torrent files with FluxDown
chinesesimplified.TorrentAssoc=将 .torrent 文件关联到 FluxDown
english.LegacyAllUsersFound=An older FluxDown installed for all users was found in:%n%1%n%nFluxDown now installs for the current user only, so future installs and updates no longer need administrator rights. Setup will remove the old copy once; Windows will ask for administrator permission this one time. The old copy is removed for every account on this PC (other accounts can install their own copy). Your downloads and settings are kept.
chinesesimplified.LegacyAllUsersFound=检测到为所有用户安装的旧版 FluxDown：%n%1%n%nFluxDown 现改为仅为当前用户安装，此后安装和更新都不再需要管理员权限。安装程序需要先卸载旧版本一次，Windows 会为此请求一次管理员权限。旧副本会对本机所有帐户移除（其他帐户可各自重新安装）。下载任务与设置会保留。
english.LegacyAllUsersRemoveFailed=The older FluxDown installed for all users in %1 could not be removed. Uninstall it from Windows Settings > Apps (administrator rights required), then run this setup again.
chinesesimplified.LegacyAllUsersRemoveFailed=未能移除 %1 中为所有用户安装的旧版 FluxDown。请在 Windows 设置 > 应用中卸载它（需要管理员权限），然后重新运行本安装程序。

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "launchonstartup"; Description: "{cm:LaunchOnStartup}"; GroupDescription: "{cm:OtherTasks}"; Flags: unchecked
Name: "torrentassoc"; Description: "{cm:TorrentAssoc}"; GroupDescription: "{cm:FileAssociations}"; Flags: unchecked

[InstallDelete]
; Upgrading from the Flutter client: drop its executable, engine DLL, plugin DLLs and
; asset bundle. A stale flux_down.exe would otherwise be relaunchable and fight
; fluxdownd for engine.lock. Runs before [Files], so the CRT DLLs are reinstalled.
Type: files; Name: "{app}\{#LegacyExeName}"
Type: files; Name: "{app}\fluxdown_updater.exe"
Type: files; Name: "{app}\*.dll"
Type: filesandordirs; Name: "{app}\data"

[Files]
Source: "{#MySourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
; First install: create desktop icon only if user checks the task
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
; Overlay/update install: always refresh the shortcut if it already exists on desktop
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Check: DesktopIconAlreadyExists

[Run]
; Setup started elevated ("Run as administrator", an elevated winget): runasoriginaluser has no
; original user to fall back to, so a direct launch would hand the administrator token to the
; desktop and the agent it starts, whose pipes the browser relay then cannot open (#951 / #787).
; `fluxdown-agent --launch-desktop` starts the desktop unelevated through the shell instead
; (native/agent/src/platform/elevation.rs).
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent shellexec; Check: not IsAdmin
Filename: "{app}\{#MyAgentExeName}"; Parameters: "--launch-desktop"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent; Check: IsAdmin
; Silent (auto-)update: relaunch the UI. `--after-update` makes the desktop wait for the
; previous instance to finish exiting instead of forwarding activation to it.
Filename: "{app}\{#MyAppExeName}"; Parameters: "--after-update"; Flags: nowait skipifdoesntexist skipifnotsilent runasoriginaluser; Check: not IsAdmin
Filename: "{app}\{#MyAgentExeName}"; Parameters: "--launch-desktop --after-update"; Flags: nowait skipifdoesntexist skipifnotsilent; Check: IsAdmin

[Registry]
; Autostart launches the resident agent (tray), same value the agent writes itself
; (native/agent/src/platform/autostart.rs).
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAgentExeName}"" --autostart"; Flags: uninsdeletevalue; Tasks: launchonstartup

; .torrent file association
Root: HKCU; Subkey: "Software\Classes\.torrent"; ValueType: string; ValueData: "FluxDown.TorrentFile"; Flags: uninsdeletekey; Tasks: torrentassoc
Root: HKCU; Subkey: "Software\Classes\FluxDown.TorrentFile"; ValueType: string; ValueData: "BitTorrent File"; Flags: uninsdeletekey; Tasks: torrentassoc
Root: HKCU; Subkey: "Software\Classes\FluxDown.TorrentFile\DefaultIcon"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"",0"; Flags: uninsdeletekey; Tasks: torrentassoc
Root: HKCU; Subkey: "Software\Classes\FluxDown.TorrentFile\shell\open\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Flags: uninsdeletekey; Tasks: torrentassoc

[UninstallDelete]
; 删除 Flutter 客户端时代的 KvStore 落盘文件（升级自 Flutter 的安装里可能仍存在）。
; 语义：卸载后重装 = 生成新设备 ID = 统计为新安装；升级/覆盖安装不触发本节，ID 保留。
; 路径 = shared_preferences_windows：%APPDATA%\<CompanyName>\<ProductName>。
Type: files; Name: "{userappdata}\FluxDown\FluxDown\shared_preferences.json"
Type: dirifempty; Name: "{userappdata}\FluxDown\FluxDown"
Type: dirifempty; Name: "{userappdata}\FluxDown"

; NMH manifest JSON files written at runtime by the agent (native/agent/src/nmh.rs)
; into the per-user data dir (never installed via [Files], so the standard
; uninstall never learns about them and leaves them on disk). The {app}
; entries cover manifests left behind by older releases that wrote them next
; to the exe.
Type: files; Name: "{localappdata}\FluxDown\nmh\com.fluxdown.nmh.json"
Type: files; Name: "{localappdata}\FluxDown\nmh\com.fluxdown.nmh.firefox.json"
Type: dirifempty; Name: "{localappdata}\FluxDown\nmh"
Type: files; Name: "{app}\com.fluxdown.nmh.json"
Type: files; Name: "{app}\com.fluxdown.nmh.firefox.json"

; 应用图标功能（agent 的 exe_icon）给 exe 打图标补丁时在安装目录留的临时副本；正常流程会自己删除，
; 这里兜底崩溃遗留，否则卸载后安装目录删不掉。
Type: files; Name: "{app}\.fluxdown-desktop.exe.icon-*.tmp"

[Code]
const
  RunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  { Uninstall key of an all-users (administrative mode) install; the GUID must
    match [Setup] AppId, Inno appends "_is1". }
  LegacyUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{B7E3F2A1-5C4D-4E8F-9A6B-1D2E3F4A5B6C}_is1';
  { Upper bound for the legacy uninstaller to finish after it was started. }
  LegacyUninstallTimeoutMs = 60000;

var
  { Per-user integrations owned by a removed all-users install; recreated
    against the new install dir once its files are in place (RemoveLegacyAllUsersInstall). }
  LegacyAutostart, LegacyTorrentAssoc, LegacyDesktopIcon: Boolean;
  LegacyFluxdownScheme, LegacyEd2kScheme, LegacyMagnetScheme: Boolean;
  { Directory of the all-users install found in HKLM ('' when none). }
  LegacyAllUsersDir: String;

function DesktopIconAlreadyExists: Boolean;
begin
  Result := LegacyDesktopIcon or FileExists(ExpandConstant('{autodesktop}\{#MyAppName}.lnk'));
end;

{ Upgrading from the Flutter client: an enabled autostart Run value still points
  at the removed flux_down.exe. Keep the user's choice by retargeting it to the
  agent (the form the agent itself writes). }
procedure MigrateLegacyAutostart;
var
  Command: String;
begin
  if not RegQueryStringValue(HKCU, RunKey, '{#MyAppName}', Command) then
    Exit;
  if Pos(Lowercase(ExpandConstant('{app}\{#LegacyExeName}')), Lowercase(Command)) > 0 then
    RegWriteStringValue(HKCU, RunKey, '{#MyAppName}',
      '"' + ExpandConstant('{app}\{#MyAgentExeName}') + '" --autostart');
end;

{ Extract the quoted executable path from a `"<exe>" "%1"`-style
  shell\open\command value (the format written by the agent,
  native/agent/src/platform/protocol_registry.rs and file_association.rs). }
function ExtractQuotedExe(const Command: String): String;
var
  FirstQuote, SecondQuote: Integer;
begin
  Result := '';
  FirstQuote := Pos('"', Command);
  if FirstQuote = 0 then Exit;
  SecondQuote := Pos('"', Copy(Command, FirstQuote + 1, MaxInt));
  if SecondQuote = 0 then Exit;
  Result := Copy(Command, FirstQuote + 1, SecondQuote - 1);
end;

{ Whether a `"<exe>" ...` command value launches Exe. }
function CommandTargets(const Command, Exe: String): Boolean;
var
  RegisteredExe: String;
begin
  RegisteredExe := ExtractQuotedExe(Command);
  Result := (RegisteredExe <> '') and (CompareText(RegisteredExe, Exe) = 0);
end;

{ fluxdown:// / ed2k:// / magnet: handler registered at runtime by
  native/agent/src/platform/protocol_registry.rs and pointing at Exe. A handler
  since reclaimed by another app (e.g. eMule re-registering ed2k://) is not ours. }
function ProtocolHandlerTargets(const Scheme, Exe: String): Boolean;
var
  Command: String;
begin
  Result := False;
  if RegQueryStringValue(HKCU, 'Software\Classes\' + Scheme + '\shell\open\command', '', Command) then
    Result := CommandTargets(Command, Exe);
end;

{ FluxDown.TorrentFile ProgID (torrentassoc task or
  native/agent/src/platform/file_association.rs) pointing at Exe. }
function TorrentAssociationTargets(const Exe: String): Boolean;
var
  Command: String;
begin
  Result := False;
  if RegQueryStringValue(HKCU, 'Software\Classes\FluxDown.TorrentFile\shell\open\command', '', Command) then
    Result := CommandTargets(Command, Exe);
end;

{ Autostart Run value (launchonstartup task or
  native/agent/src/platform/autostart.rs, `"<fluxdown-agent.exe>" --autostart`)
  pointing at Agent. }
function AutostartTargets(const Agent: String): Boolean;
var
  Command, RegisteredExe: String;
begin
  Result := False;
  if not RegQueryStringValue(HKCU, RunKey, '{#MyAppName}', Command) then
    Exit;
  RegisteredExe := ExtractQuotedExe(Command);
  { Legacy entries may store the path unquoted; fall back to the raw value
    with any trailing arguments stripped. }
  if RegisteredExe = '' then
  begin
    RegisteredExe := Trim(Command);
    if Pos(' --', RegisteredExe) > 0 then
      RegisteredExe := Trim(Copy(RegisteredExe, 1, Pos(' --', RegisteredExe) - 1));
  end;
  Result := (RegisteredExe <> '') and (CompareText(RegisteredExe, Agent) = 0);
end;

{ Same values as native/agent/src/platform/protocol_registry.rs::register. }
procedure WriteProtocolHandler(const Scheme, Description, Exe: String);
var
  Key: String;
begin
  Key := 'Software\Classes\' + Scheme;
  RegWriteStringValue(HKCU, Key, '', Description);
  RegWriteStringValue(HKCU, Key, 'URL Protocol', '');
  RegWriteStringValue(HKCU, Key + '\DefaultIcon', '', '"' + Exe + '",0');
  RegWriteStringValue(HKCU, Key + '\shell\open\command', '', '"' + Exe + '" "%1"');
end;

{ Same values as the torrentassoc [Registry] entries and
  native/agent/src/platform/file_association.rs::associate. }
procedure WriteTorrentAssociation(const Exe: String);
begin
  RegWriteStringValue(HKCU, 'Software\Classes\.torrent', '', 'FluxDown.TorrentFile');
  RegWriteStringValue(HKCU, 'Software\Classes\FluxDown.TorrentFile', '', 'BitTorrent File');
  RegWriteStringValue(HKCU, 'Software\Classes\FluxDown.TorrentFile\DefaultIcon', '', '"' + Exe + '",0');
  RegWriteStringValue(HKCU, 'Software\Classes\FluxDown.TorrentFile\shell\open\command', '', '"' + Exe + '" "%1"');
end;

{ Earlier releases offered "install for all users" (Program Files + HKLM
  uninstall key). Setup is per-user only now, and leaving that copy behind would
  keep an older fluxdown-agent/fluxdownd reachable through its shortcuts,
  competing for the same %LOCALAPPDATA%\FluxDown data dir, and keep the browser
  NMH registration pinned to its relay (auto_register in native/agent/src/nmh.rs
  never takes over from another healthy install). Remove it through its own
  uninstaller — the one elevation left — after snapshotting the per-user
  integrations that uninstaller deletes (CurUninstallStepChanged), so
  RestoreLegacyIntegrations can recreate them against the new install dir. NMH needs no
  snapshot: its keys go missing and the new agent re-registers on first launch.

  The legacy copy counts as removed once no UI entry point (GPUI desktop or
  Flutter exe) is left in its directory, not once the HKLM uninstall key is gone:
  Inno deletes that key last, and a slow or interrupted second uninstall phase
  used to abort setup after the old files were already deleted, leaving the
  user with no FluxDown at all (#912). Leftover agent/daemon files are
  unreachable by then (the uninstaller drops their shortcuts, Run value and
  handlers first), and a stale key is skipped on the next run. Setup aborts
  only while the old copy can still be launched, so the user always keeps one
  working install. }
function LegacyLaunchable(const Dir: String): Boolean;
begin
  Result := FileExists(Dir + '\{#MyAppExeName}') or FileExists(Dir + '\{#LegacyExeName}');
end;

function LegacyFilesLeft(const Dir: String): Boolean;
begin
  Result := LegacyLaunchable(Dir)
    or FileExists(Dir + '\{#MyAgentExeName}')
    or FileExists(Dir + '\fluxdownd.exe')
    or FileExists(Dir + '\fluxdown_nmh.exe');
end;

function RemoveLegacyAllUsersInstall: String;
var
  UninstallString, UninstallExe, LegacyDir, LegacyDesktop: String;
  ErrorCode, Waited: Integer;
begin
  Result := '';
  if not RegQueryStringValue(HKLM, LegacyUninstallKey, 'UninstallString', UninstallString) then
    Exit;
  UninstallExe := RemoveQuotes(UninstallString);
  LegacyDir := ExtractFileDir(UninstallExe);
  LegacyAllUsersDir := LegacyDir;
  LegacyDesktop := LegacyDir + '\{#MyAppExeName}';
  LegacyAutostart := AutostartTargets(LegacyDir + '\{#MyAgentExeName}');
  LegacyTorrentAssoc := TorrentAssociationTargets(LegacyDesktop);
  LegacyFluxdownScheme := ProtocolHandlerTargets('fluxdown', LegacyDesktop);
  LegacyEd2kScheme := ProtocolHandlerTargets('ed2k', LegacyDesktop);
  LegacyMagnetScheme := ProtocolHandlerTargets('magnet', LegacyDesktop);
  LegacyDesktopIcon := FileExists(ExpandConstant('{commondesktop}\{#MyAppName}.lnk'));
  { Already removed (by hand or by an earlier, interrupted run): only a stale
    HKLM entry or unreachable leftovers remain, so no elevation is needed. }
  if not LegacyLaunchable(LegacyDir) then
  begin
    Log('Legacy all-users install in ' + LegacyDir + ' is already removed; skipping its uninstaller.');
    Exit;
  end;
  { Uninstaller deleted but the program left in place: nothing can remove it
    without admin rights, so install alongside it rather than block updates. }
  if not FileExists(UninstallExe) then
  begin
    Log('Legacy all-users install in ' + LegacyDir + ' has no uninstaller; installing alongside it.');
    Exit;
  end;

  if SuppressibleMsgBox(FmtMessage(CustomMessage('LegacyAllUsersFound'), [LegacyDir]),
    mbInformation, MB_OKCANCEL, IDOK) <> IDOK then
  begin
    Result := FmtMessage(CustomMessage('LegacyAllUsersRemoveFailed'), [LegacyDir]);
    Exit;
  end;
  { UAC is shown even for /VERYSILENT; declining it makes ShellExec fail. }
  if not ShellExec('runas', UninstallExe, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '',
    SW_HIDE, ewWaitUntilTerminated, ErrorCode) then
  begin
    Log(Format('Legacy uninstaller could not be started (error %d).', [ErrorCode]));
    Result := FmtMessage(CustomMessage('LegacyAllUsersRemoveFailed'), [LegacyDir]);
    Exit;
  end;
  { The first uninstall phase may return before the second phase is done;
    wait (bounded) until both the files and the uninstall key are gone. }
  Waited := 0;
  while (LegacyFilesLeft(LegacyDir) or RegKeyExists(HKLM, LegacyUninstallKey))
    and (Waited < LegacyUninstallTimeoutMs) do
  begin
    Sleep(250);
    Waited := Waited + 250;
  end;
  if LegacyLaunchable(LegacyDir) then
  begin
    Log(Format('Legacy uninstaller exited with code %d, but %s is still launchable after %d ms.', [ErrorCode, LegacyDir, Waited]));
    Result := FmtMessage(CustomMessage('LegacyAllUsersRemoveFailed'), [LegacyDir]);
    Exit;
  end;
  if LegacyFilesLeft(LegacyDir) or RegKeyExists(HKLM, LegacyUninstallKey) then
    Log('Legacy install in ' + LegacyDir + ' can no longer be launched; continuing despite leftover files or uninstall key.')
  else
    Log('Legacy all-users install in ' + LegacyDir + ' removed.');
end;

{ PrepareToInstall force-kills every FluxDown process before the legacy
  migration. When that migration aborts a silent (auto-)update, nothing would
  bring the app back: the agent that started setup has already exited. Start
  whichever install is still there so the user is not left without FluxDown.
  This runs before [Files], so the agent on disk is the previous release that
  does not know `--launch-desktop`: launch the desktop directly. }
procedure RelaunchAfterAbortedUpdate;
var
  ResultCode: Integer;
  Exe, Params: String;
begin
  Params := '--relaunch';
  Exe := ExpandConstant('{app}\{#MyAppExeName}');
  if not FileExists(Exe) and (LegacyAllUsersDir <> '') then
    Exe := LegacyAllUsersDir + '\{#MyAppExeName}';
  if not FileExists(Exe) and (LegacyAllUsersDir <> '') then
  begin
    { Flutter-era client: no GPUI launch flags. }
    Exe := LegacyAllUsersDir + '\{#LegacyExeName}';
    Params := '';
  end;
  if not FileExists(Exe) then
  begin
    Log('No FluxDown executable left to relaunch after the aborted update.');
    Exit;
  end;
  if ExecAsOriginalUser(Exe, Params, '', SW_SHOWNORMAL, ewNoWait, ResultCode) then
    Log('Relaunched ' + Exe + ' after the aborted update.')
  else
    Log(Format('Could not relaunch %s after the aborted update (error %d).', [Exe, ResultCode]));
end;

procedure RestoreLegacyIntegrations;
var
  Desktop: String;
begin
  Desktop := ExpandConstant('{app}\{#MyAppExeName}');
  { Only the Run value is rewritten; StartupApproved (system-level disable) is
    left as the user set it, like autostart::retarget. }
  if LegacyAutostart then
    RegWriteStringValue(HKCU, RunKey, '{#MyAppName}',
      '"' + ExpandConstant('{app}\{#MyAgentExeName}') + '" --autostart');
  if LegacyTorrentAssoc then
    WriteTorrentAssociation(Desktop);
  if LegacyFluxdownScheme then
    WriteProtocolHandler('fluxdown', 'URL:FluxDown Protocol', Desktop);
  if LegacyEd2kScheme then
    WriteProtocolHandler('ed2k', 'URL:ed2k Protocol', Desktop);
  if LegacyMagnetScheme then
    WriteProtocolHandler('magnet', 'URL:Magnet Link', Desktop);
end;

{ Upgrading from the Flutter client: .torrent association and URL protocol handlers
  still point at the removed flux_down.exe. Retarget only entries that name this
  install's old exe, so other programs' registrations stay untouched. }
procedure MigrateLegacyIntegrations;
var
  OldExe, NewExe: String;
begin
  OldExe := ExpandConstant('{app}\{#LegacyExeName}');
  NewExe := ExpandConstant('{app}\{#MyAppExeName}');
  if TorrentAssociationTargets(OldExe) then
    WriteTorrentAssociation(NewExe);
  if ProtocolHandlerTargets('fluxdown', OldExe) then
    WriteProtocolHandler('fluxdown', 'URL:FluxDown Protocol', NewExe);
  if ProtocolHandlerTargets('ed2k', OldExe) then
    WriteProtocolHandler('ed2k', 'URL:ed2k Protocol', NewExe);
  if ProtocolHandlerTargets('magnet', OldExe) then
    WriteProtocolHandler('magnet', 'URL:Magnet Link', NewExe);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  { Force-kill every FluxDown process as a fallback in case Restart Manager
    fails: the resident agent/daemon/relay keep their exes locked otherwise.
    Matches by image name, so a legacy all-users copy is stopped too. }
  Exec('taskkill', '/f /im {#MyAppExeName} /im {#MyAgentExeName} /im fluxdownd.exe /im fluxdown_nmh.exe /im {#LegacyExeName}', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  { Upgrade installs must overwrite the previous uninstaller; a stray
    read-only attribute on it makes CreateFile fail with access denied.
    Clear the attribute up-front (no-op when the files do not exist). }
  Exec('attrib', '-r "' + ExpandConstant('{app}\unins000.exe') + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec('attrib', '-r "' + ExpandConstant('{app}\unins000.dat') + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  { Small delay to ensure file locks are released }
  Sleep(500);
  Result := RemoveLegacyAllUsersInstall;
  if (Result <> '') and WizardSilent then
    RelaunchAfterAbortedUpdate;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    MigrateLegacyAutostart;
    MigrateLegacyIntegrations;
    RestoreLegacyIntegrations;
  end;
end;

{ Remove a URL scheme handler registered at runtime. These keys live under
  HKCU\Software\Classes\<scheme> and are never declared in [Registry] — the
  standard uninstall never removes them, and Windows tries to relaunch the
  deleted exe whenever a matching link is opened. }
procedure RemoveProtocolHandler(const Scheme: String);
begin
  if ProtocolHandlerTargets(Scheme, ExpandConstant('{app}\{#MyAppExeName}')) then
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\' + Scheme);
end;

{ Remove the `.torrent` file association registered at runtime (toggled from the
  app's settings page, 独立于 install-time 的 torrentassoc task — that task's registry
  entries carry uninsdeletekey, but a runtime-written association is
  invisible to the uninstall log). Only removes the `.torrent` extension key if
  it still maps to our ProgID (mirrors the conservative ownership check in
  file_association.rs::disassociate). }
procedure RemoveTorrentAssociation;
var
  ProgId: String;
begin
  if not TorrentAssociationTargets(ExpandConstant('{app}\{#MyAppExeName}')) then
    Exit;
  if RegQueryStringValue(HKCU, 'Software\Classes\.torrent', '', ProgId)
    and (CompareText(ProgId, 'FluxDown.TorrentFile') = 0) then
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\.torrent');
  RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\FluxDown.TorrentFile');
end;

{ Remove the autostart Run value written at runtime by the agent. A
  runtime-written value is invisible to the uninstall log, so uninsdeletevalue
  alone cannot be relied on. }
procedure RemoveAutostartRunValue;
begin
  if AutostartTargets(ExpandConstant('{app}\{#MyAgentExeName}')) then
    RegDeleteValue(HKCU, RunKey, '{#MyAppName}');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    { Chrome/Edge/Firefox Native Messaging Host registrations written at
      runtime by the agent (native/agent/src/nmh.rs). Never declared in the
      Registry section (the app writes them directly via winreg on every startup),
      so the standard uninstall never removes them. `com.fluxdown.nmh` is
      FluxDown-specific, safe to remove unconditionally. }
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Google\Chrome\NativeMessagingHosts\com.fluxdown.nmh');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Microsoft\Edge\NativeMessagingHosts\com.fluxdown.nmh');
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Mozilla\NativeMessagingHosts\com.fluxdown.nmh');

    { fluxdown:// / ed2k:// / magnet: URL protocol handlers — same gap as above. }
    RemoveProtocolHandler('fluxdown');
    RemoveProtocolHandler('ed2k');
    RemoveProtocolHandler('magnet');

    { Completion notifications target the agent, not the desktop executable.
      Leave a handler installed by another copy (including portable) untouched. }
    if ProtocolHandlerTargets('fluxdown-notification', ExpandConstant('{app}\{#MyAgentExeName}')) then
      RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\fluxdown-notification');

    { .torrent association + autostart Run value — runtime-written variants
      of the [Registry] task entries, invisible to the uninstall log. }
    RemoveTorrentAssociation;
    RemoveAutostartRunValue;
  end;
end;
