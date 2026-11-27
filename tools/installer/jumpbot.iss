#ifndef PayloadDir
  #define PayloadDir "..\..\dist\jumpbot"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\installer"
#endif
#ifndef AppVersion
  #define AppVersion "1.2"
#endif
#ifndef AppVersionInfo
  #define AppVersionInfo "1.2.0.0"
#endif
#ifndef AppFlavor
  #define AppFlavor "full"
#endif
#if AppFlavor == "offline"
  #define AppTitle "跳一跳控制台"
  #define AppSubdir "jumpbot-offline-preview"
  #define PreviewGuid "{AA105DEB-419A-4E5B-A840-4F7E2332A61C}"
  #define AppIdentity "{" + PreviewGuid
#elif AppFlavor == "portable"
  #define AppTitle "跳一跳自动脚本（兼容轻量版）"
  #define AppSubdir "jumpbot-portable"
  #define AppIdentity "{{B5B2F778-6F4B-4B39-A24D-707780C50259}"
#else
  #define AppTitle "跳一跳自动脚本"
  #define AppSubdir "jumpbot"
  #define AppIdentity "{{8F3A2C41-7B5E-4D19-9C6A-2E48D1F0B7A3}"
#endif

[Setup]
AppId={#AppIdentity}
AppName={#AppTitle}
AppVersion={#AppVersion}
AppVerName={#AppTitle} {#AppVersion}
AppPublisher=chadqian2012-sudo
#if AppFlavor != "offline"
AppPublisherURL=https://github.com/chadqian2012-sudo/jumpbot
AppSupportURL=https://github.com/chadqian2012-sudo/jumpbot
#endif
VersionInfoVersion={#AppVersionInfo}
VersionInfoDescription={#AppTitle} 安装程序
DefaultDirName={localappdata}\Programs\{#AppSubdir}
#if AppFlavor == "offline"
DefaultGroupName=JumpBot
#else
DefaultGroupName={#AppTitle}
#endif
PrivilegesRequired=lowest
#if AppFlavor == "offline"
DisableProgramGroupPage=no
DisableDirPage=no
DisableWelcomePage=no
#else
DisableProgramGroupPage=yes
PrivilegesRequiredOverridesAllowed=dialog
#endif
OutputDir={#OutputDir}
OutputBaseFilename=jumpbot-setup-{#AppVersion}-{#AppFlavor}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
#if AppFlavor == "offline"
WizardImageFile=wizard-brand.png
#endif
WizardSmallImageFile=..\..\desktop\ui\JumpBot-Interval.bmp
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppTitle}
SetupIconFile=..\..\desktop\ui\JumpBot-Interval.ico
UninstallDisplayIcon={app}\jumpbot-desktop.exe
SetupLogging=yes
AllowNoIcons=yes

[Languages]
Name: "chinese"; MessagesFile: "ChineseSimplified.isl"

#if AppFlavor == "offline"
[LangOptions]
DialogFontName=Microsoft YaHei UI
WelcomeFontName=Microsoft YaHei UI
WelcomeFontSize=18

[Types]
Name: "all"; Description: "自动选择（推荐）"
Name: "cpu"; Description: "仅 CPU"
Name: "custom"; Description: "自定义"; Flags: iscustom

[Components]
Name: "core"; Description: "主程序与运行库（必选）"; Types: all cpu custom; Flags: fixed
Name: "cuda"; Description: "CUDA（NVIDIA）"; Types: all
Name: "opencl"; Description: "OpenCL（Intel、AMD、NVIDIA）"; Types: all

[Messages]
WelcomeLabel1=[name]
WelcomeLabel2=选择安装位置和加速方式。%n所需组件已随安装包提供。
ClickNext=
ButtonNext=下一步(&N)
ButtonBack=上一步(&B)
WizardSelectDir=安装位置
SelectDirDesc=选择程序的安装文件夹。
SelectDirLabel3=安装文件夹
SelectDirBrowseLabel=首次安装请选择空文件夹，升级请沿用原目录。
WizardSelectComponents=加速方式
SelectComponentsDesc=按这台电脑的硬件选择。
SelectComponentsLabel2=默认启用可用的显卡加速，没有兼容显卡时使用 CPU。
WizardSelectTasks=快捷方式
SelectTasksDesc=选择从哪里打开控制台。
SelectTasksLabel2=
WizardReady=确认安装
ReadyLabel1=请确认以下设置。
ReadyLabel2a=
ReadyLabel2b=
ReadyMemoTasks=快捷方式
FinishedHeadingLabel=安装完成
FinishedLabel=[name]已就绪。
FinishedLabelNoIcons=[name]已就绪。
ClickFinish=

[INI]
Filename: "{app}\.jumpbot-offline-preview.ini"; Section: "Install"; Key: "Channel"; String: "offline-preview"

[InstallDelete]
Type: files; Name: "{group}\NVIDIA CUDA 模式.lnk"; Check: not WizardIsComponentSelected('cuda')
Type: files; Name: "{group}\OpenCL 模式.lnk"; Check: not WizardIsComponentSelected('opencl')
Type: files; Name: "{autodesktop}\跳一跳控制台（离线测试版）.lnk"; Check: (not WizardIsTaskSelected('desktopicon') or DesktopNameFree) and DesktopPointsToConsole('跳一跳控制台（离线测试版）')
Type: files; Name: "{autodesktop}\JumpBot 控制台.lnk"; Check: not WizardIsTaskSelected('desktopicon') and DesktopPointsToConsole('JumpBot 控制台')
#endif

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; Flags: checkedonce

[Files]
; A v1.3 uninstall log can remove config.json. Restore saved user bytes first.
Source: "{app}\config.jumpbot-preserved.json"; DestDir: "{app}"; DestName: "config.json"; Flags: external skipifsourcedoesntexist onlyifdoesntexist uninsneveruninstall
; At install time, preserve digit templates harvested with v1.3.
Source: "{app}\_internal\templates\*.png"; DestDir: "{app}\templates"; Flags: external recursesubdirs skipifsourcedoesntexist onlyifdoesntexist uninsneveruninstall
#include PayloadFiles
#if AppFlavor == "offline"
Source: "{#PayloadDir}\{#CvDll}"; DestDir: "{app}"; Hash: "{#CvHash}"; Flags: ignoreversion; Check: not UseLocalOpenCV; BeforeInstall: Log('OpenCV file entry: bundled')
Source: "{code:VerifiedOpenCVFile}"; DestDir: "{app}"; DestName: "{#CvDll}"; ExternalSize: {#CvSize}; Hash: "{#CvHash}"; Flags: external ignoreversion; Check: UseLocalOpenCV; BeforeInstall: Log('OpenCV file entry: local')
#endif

[Icons]
#if AppFlavor == "offline"
Name: "{group}\NVIDIA CUDA 模式"; Filename: "{app}\jumpbot.exe"; Parameters: "--backend cuda --max-jumps 0 --daemon"; WorkingDir: "{app}"; Components: cuda
Name: "{group}\OpenCL 模式"; Filename: "{app}\jumpbot.exe"; Parameters: "--backend opencl --max-jumps 0 --daemon"; WorkingDir: "{app}"; Components: opencl
#endif
Name: "{group}\跳一跳控制台"; Filename: "{app}\jumpbot-desktop.exe"; WorkingDir: "{app}"
Name: "{group}\跳一跳自动脚本（长跑）"; Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; WorkingDir: "{app}"; Comment: "不限跳数，掉窗口自动等回来；F8 停止"
Name: "{group}\自检（不碰游戏）"; Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; WorkingDir: "{app}"; Comment: "检查依赖 / 模板 / GPU 后端，不发送任何输入"
Name: "{group}\纯 CPU 模式"; Filename: "{app}\jumpbot.exe"; Parameters: "--backend simd --no-cuda --no-opencl --max-jumps 0 --daemon"; WorkingDir: "{app}"
Name: "{group}\只识别不发输入（安全演示）"; Filename: "{app}\jumpbot.exe"; Parameters: "--dry-run --samples 3"; WorkingDir: "{app}"
Name: "{group}\配置文件夹"; Filename: "{app}"
Name: "{group}\卸载 跳一跳自动脚本"; Filename: "{uninstallexe}"
#if AppFlavor == "offline"
Name: "{autodesktop}\JumpBot 控制台"; Filename: "{app}\jumpbot-desktop.exe"; WorkingDir: "{app}"; Tasks: desktopicon; Check: DesktopNameFree
#else
Name: "{autodesktop}\{#AppTitle}"; Filename: "{app}\jumpbot-desktop.exe"; WorkingDir: "{app}"; Tasks: desktopicon
#endif

[Run]
Filename: "{app}\run.exe"; Parameters: "--finish-config-restore"; Flags: runhidden
Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; Description: "运行自检（推荐，验证依赖与 GPU 后端）"; Flags: postinstall nowait skipifsilent
Filename: "{app}\jumpbot-desktop.exe"; Description: "打开桌面控制台"; Flags: postinstall nowait skipifsilent unchecked

[UninstallRun]
Filename: "{app}\run.exe"; Parameters: "--preserve-uninstall-config"; Flags: runhidden; RunOnceId: "PreserveJumpBotConfig"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\logs"
Type: filesandordirs; Name: "{app}\debug"

#if AppFlavor == "offline"
[Code]
var
  CvSource: TInputOptionWizardPage;
  CvFile: TInputFileWizardPage;
  CvSnapshot: String;

function DesktopNameFree: Boolean;
var
  Base: String;
begin
  Base := ExpandConstant('{autodesktop}\JumpBot 控制台');
  // Inno replaces .pif, .url and folder shortcuts of the same name as well.
  Result := not (FileExists(Base + '.lnk') or FileExists(Base + '.pif') or
    FileExists(Base + '.url') or DirExists(Base) or DirExists(Base + '.lnk'));
end;

function LinkPointsTo(Path, Expected, Args: String): Boolean;
var
  Shell, Folder, Item, Link: Variant;
  Target: String;
begin
  Result := False;
  if not FileExists(Path) then Exit;
  try
    Shell := CreateOleObject('Shell.Application');
    Folder := Shell.NameSpace(ExtractFileDir(Path));
    Item := Folder.ParseName(ExtractFileName(Path));
    Link := Item.GetLink;
    Target := Link.Path;
    Result := (String(Link.Arguments) = Args) and ((CompareText(Target, Expected) = 0) or
      ((Target <> '') and (CompareText(Target, GetShortName(Expected)) = 0)));
  except
    Log(GetExceptionMessage);
  end;
end;

function DesktopPointsToConsole(Name: String): Boolean;
begin
  Result := LinkPointsTo(ExpandConstant('{autodesktop}\') + Name + '.lnk',
    ExpandConstant('{app}\jumpbot-desktop.exe'), '');
end;

#define LegacyGroup "跳一跳控制台（离线测试版）"
var
  LegacyInstallDir: String;

<event('InitializeWizard')>
procedure RenameDefaultPreviewGroup;
var
  Key, Group: String;
begin
  Key := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#PreviewGuid}_is1';
  if not RegQueryStringValue(HKCU64, Key, 'Inno Setup: Icon Group', Group) then Exit;
  if Group <> '{#LegacyGroup}' then Exit;
  if not RegQueryStringValue(HKCU64, Key, 'Inno Setup: App Path', LegacyInstallDir) then Exit;
  if WizardForm.GroupEdit.Text = Group then WizardForm.GroupEdit.Text := 'JumpBot';
end;

procedure RemoveLegacyMenuLink(Name, Expected, Args: String);
var
  Path: String;
begin
  Path := ExpandConstant('{userprograms}\{#LegacyGroup}\') + Name + '.lnk';
  if LinkPointsTo(Path, Expected, Args) then DeleteFile(Path);
end;

<event('CurStepChanged')>
procedure CleanPreviousDefaultGroup(Step: TSetupStep);
var
  Group, AppDir, Worker: String;
  Info: TFindRec;
begin
  if Step <> ssPostInstall then Exit;
  AppDir := ExpandConstant('{app}');
  if (CompareText(LegacyInstallDir, AppDir) <> 0) or (WizardGroupValue = '{#LegacyGroup}') then Exit;
  Group := ExpandConstant('{userprograms}\{#LegacyGroup}');
  if not FindFirst(Group, Info) then Exit;
  FindClose(Info);
  if (Info.Attributes and $400) <> 0 then Exit; // Do not follow a junction.
  Worker := AddBackslash(AppDir) + 'jumpbot.exe';
  RemoveLegacyMenuLink('跳一跳控制台', AddBackslash(AppDir) + 'jumpbot-desktop.exe', '');
  RemoveLegacyMenuLink('NVIDIA CUDA 模式', Worker, '--backend cuda --max-jumps 0 --daemon');
  RemoveLegacyMenuLink('OpenCL 模式', Worker, '--backend opencl --max-jumps 0 --daemon');
  RemoveLegacyMenuLink('跳一跳自动脚本（长跑）', Worker, '--max-jumps 0 --daemon');
  RemoveLegacyMenuLink('自检（不碰游戏）', Worker, '--selftest');
  RemoveLegacyMenuLink('纯 CPU 模式', Worker, '--backend simd --no-cuda --no-opencl --max-jumps 0 --daemon');
  RemoveLegacyMenuLink('只识别不发输入（安全演示）', Worker, '--dry-run --samples 3');
  RemoveLegacyMenuLink('配置文件夹', AppDir, '');
  RemoveLegacyMenuLink('卸载 跳一跳自动脚本', ExpandConstant('{uninstallexe}'), '');
  RemoveDir(Group); // Windows removes empty directories only.
end;

function GetDriveTypeW(Root: String): Cardinal;
  external 'GetDriveTypeW@kernel32.dll stdcall';

function VerifiedOpenCVFile(Param: String): String;
begin
  Result := CvSnapshot;
end;

procedure InitializeWizard;
var
  Choice: String;
begin
  with WizardForm.WelcomeLabel1 do
    SetBounds(Left + ScaleX(14), WizardForm.WelcomePage.ClientHeight * 2 div 5,
      Width - ScaleX(14), ScaleY(34));
  with WizardForm.WelcomeLabel2 do
    SetBounds(WizardForm.WelcomeLabel1.Left, WizardForm.WelcomeLabel1.Top + ScaleY(38),
      WizardForm.WelcomeLabel1.Width, WizardForm.WelcomePage.ClientHeight div 3);
  CvSource := CreateInputOptionPage(wpSelectComponents, 'OpenCV 运行库',
    '选择图像识别使用的运行库。', '首次安装建议使用内置版本。', True, False);
  CvSource.Add('使用内置版本（推荐）');
  CvSource.Add('选择本机已有的 DLL');
  CvFile := CreateInputFilePage(CvSource.ID, '本机 OpenCV 文件',
    '选择与内置版本一致的 x64 DLL。',
    '选择 {#CvDll}，或留空使用安装目录中的同名文件。');
  CvFile.Add('OpenCV 文件', '动态链接库 (*.dll)|*.dll|所有文件 (*.*)|*.*', '.dll');
  Choice := ExpandConstant('{param:OPENCV|bundled}');
  CvSource.SelectedValueIndex := 0;
  if CompareText(Choice, 'bundled') <> 0 then
  begin
    CvSource.SelectedValueIndex := 1;
    if CompareText(Choice, 'installed') <> 0 then CvFile.Values[0] := Choice;
  end;
end;

function UseLocalOpenCV: Boolean;
begin
  Result := CvSource.SelectedValueIndex = 1;
end;

function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo, MemoTypeInfo,
  MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
begin
  Result := MemoDirInfo + NewLine + NewLine + MemoComponentsInfo + NewLine + Space;
  if UseLocalOpenCV then Result := Result + 'OpenCV（本机文件）'
  else Result := Result + 'OpenCV（安装包内置）';
  if MemoGroupInfo <> '' then Result := Result + NewLine + NewLine + MemoGroupInfo;
  if MemoTasksInfo <> '' then Result := Result + NewLine + NewLine + MemoTasksInfo;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = wpSelectProgramGroup) or ((PageID = CvFile.ID) and not UseLocalOpenCV);
end;

function CheckLocalOpenCV(MakeSnapshot: Boolean): String;
var
  Src, ToCheck: String;
  Size: Int64;
begin
  Src := Trim(CvFile.Values[0]);
  if Src = '' then Src := AddBackslash(WizardDirValue) + '{#CvDll}';
  StringChangeEx(Src, '/', '\', True);
  if (Length(Src) < 4) or (Copy(Src, 2, 2) <> ':\') or
     (GetDriveTypeW(Copy(Src, 1, 3)) = 4) then
    Result := '请选择本地磁盘上的 DLL 文件。'
  else if not FileSize64(Src, Size) then
    Result := '找不到或无法读取所选 DLL。请检查文件路径和读取权限。'
  else if Size <> {#CvSize} then
    Result := '所选 DLL 的大小与内置 {#CvDll} 不符。'
  else
    try
      ToCheck := Src;
      if MakeSnapshot then
      begin
        ToCheck := GenerateUniqueName(ExpandConstant('{tmp}'), '.bin');
        if not CopyFile(Src, ToCheck, False) then
          RaiseException('无法复制到安装临时目录，请检查权限和剩余空间。');
      end;
      if CompareText(GetSHA256OfFile(ToCheck), '{#CvHash}') <> 0 then
        Result := '所选 DLL 的 SHA-256 与内置 {#CvDll} 不符。'
      else if MakeSnapshot then
      begin
        CvSnapshot := ToCheck;
        Log('OpenCV source: local | ' + Src);
      end;
    except
      Result := '无法准备 OpenCV 文件。' + GetExceptionMessage;
    end;
  if Result <> '' then
  begin
    Result := Result + ' 请重新选文件，或返回使用内置 OpenCV。';
    Log('OpenCV: ' + Result);
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Error: String;
begin
  Result := True;
  if (CurPageID = CvFile.ID) and UseLocalOpenCV and not WizardSilent then
  begin
    Error := CheckLocalOpenCV(False);
    Result := Error = '';
    if not Result then MsgBox(Error, mbError, MB_OK);
  end;
end;

function CheckInstallDirectory: String;
var
  Dir, Previous: String;
  Item: TFindRec;
begin
  Dir := WizardDirValue;
  if RegQueryStringValue(HKCU64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#PreviewGuid}_is1',
      'Inno Setup: App Path', Previous) and (CompareText(RemoveBackslashUnlessRoot(Previous), RemoveBackslashUnlessRoot(Dir)) <> 0) then
  begin
    Result := '控制台已安装到 ' + Previous + '。请在原目录升级，或先卸载再更换目录。';
    Exit;
  end;
  if GetIniString('Install', 'Channel', '', AddBackslash(Dir) + '.jumpbot-offline-preview.ini') = 'offline-preview' then Exit;
  if FindFirst(AddBackslash(Dir) + '*', Item) then
  try
    repeat
      if (Item.Name <> '.') and (Item.Name <> '..') then
      begin
        Result := '此文件夹已包含其他文件。请选择空文件夹，或沿用控制台原安装目录。';
        Exit;
      end;
    until not FindNext(Item);
  finally
    FindClose(Item);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := CheckInstallDirectory;
  if Result <> '' then Exit;
  if UseLocalOpenCV then
    Result := CheckLocalOpenCV(True)
  else
    Log('OpenCV source: bundled | {#CvDll}');
end;
#endif
