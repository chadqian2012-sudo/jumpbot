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
  #define AppTitle "跳一跳控制台（离线测试版）"
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
DefaultGroupName={#AppTitle}
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
[Types]
Name: "all"; Description: "自动选择（CPU、CUDA 和 OpenCL）"
Name: "cpu"; Description: "仅 CPU（兼容模式）"
Name: "custom"; Description: "自选加速组件"; Flags: iscustom

[Components]
Name: "core"; Description: "主程序、CPU、OpenCV 和 Visual C++ 运行库（必装）"; Types: all cpu custom; Flags: fixed
Name: "cuda"; Description: "NVIDIA CUDA 加速"; Types: all
Name: "opencl"; Description: "OpenCL 加速（Intel、AMD 或 NVIDIA）"; Types: all

[Messages]
SelectComponentsLabel2=选择可使用的加速方式，然后点击“下一步”。已有兼容驱动时可使用显卡，没有合适设备时自动使用 CPU。无需下载驱动或 CUDA Toolkit。

[INI]
Filename: "{app}\.jumpbot-offline-preview.ini"; Section: "Install"; Key: "Channel"; String: "offline-preview"

[InstallDelete]
Type: files; Name: "{group}\NVIDIA CUDA 模式.lnk"; Check: not WizardIsComponentSelected('cuda')
Type: files; Name: "{group}\OpenCL 模式.lnk"; Check: not WizardIsComponentSelected('opencl')
Type: files; Name: "{autodesktop}\{#AppTitle}.lnk"; Check: not WizardIsTaskSelected('desktopicon')
#endif

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: checkedonce

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
Name: "{autodesktop}\{#AppTitle}"; Filename: "{app}\jumpbot-desktop.exe"; WorkingDir: "{app}"; Tasks: desktopicon

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
  CvSource := CreateInputOptionPage(wpSelectComponents, '选择 OpenCV 来源',
    '程序必须使用 OpenCV', '安装程序会将所选 DLL 放在本程序目录，系统 PATH 保持原样。', True, False);
  CvSource.Add('使用安装包内置的 OpenCV（推荐，离线可用）');
  CvSource.Add('使用本机已有的兼容 OpenCV（高级）');
  CvFile := CreateInputFilePage(CvSource.ID, '选择本机 OpenCV 文件',
    '只接受与内置 x64 版本完全相同的文件',
    '请选择本地 DLL，或留空以使用安装目录中已有的 {#CvDll}。' + #13#10 +
    '安装前会按 SHA-256 校验。可点击“上一步”改用内置版本。');
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
    Result := '测试版已安装到 ' + Previous + '。请在原目录升级，或先卸载测试版再更换目录。';
    Exit;
  end;
  if GetIniString('Install', 'Channel', '', AddBackslash(Dir) + '.jumpbot-offline-preview.ini') = 'offline-preview' then Exit;
  if FindFirst(AddBackslash(Dir) + '*', Item) then
  try
    repeat
      if (Item.Name <> '.') and (Item.Name <> '..') then
      begin
        Result := '请选择空文件夹或已有的离线测试版目录，以免覆盖正式版或其他文件。';
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
