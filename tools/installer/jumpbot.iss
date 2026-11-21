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
#if AppFlavor == "portable"
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
AppPublisherURL=https://github.com/chadqian2012-sudo/jumpbot
AppSupportURL=https://github.com/chadqian2012-sudo/jumpbot
VersionInfoVersion={#AppVersionInfo}
VersionInfoDescription={#AppTitle} 安装程序
DefaultDirName={localappdata}\Programs\{#AppSubdir}
DefaultGroupName={#AppTitle}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir={#OutputDir}
OutputBaseFilename=jumpbot-setup-{#AppVersion}-{#AppFlavor}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#AppTitle}
UninstallDisplayIcon={app}\jumpbot-desktop.exe
SetupLogging=yes
AllowNoIcons=yes

[Languages]
Name: "chinese"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: checkedonce

[Files]
; A v1.3 uninstall log can remove config.json. Restore saved user bytes first.
Source: "{app}\config.jumpbot-preserved.json"; DestDir: "{app}"; DestName: "config.json"; Flags: external skipifsourcedoesntexist onlyifdoesntexist uninsneveruninstall
; At install time, preserve digit templates harvested with v1.3.
Source: "{app}\_internal\templates\*.png"; DestDir: "{app}\templates"; Flags: external recursesubdirs skipifsourcedoesntexist onlyifdoesntexist uninsneveruninstall
#include PayloadFiles

[Icons]
Name: "{group}\跳一跳控制台"; Filename: "{app}\jumpbot-desktop.exe"; WorkingDir: "{app}"
Name: "{group}\跳一跳自动脚本（长跑）"; Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; WorkingDir: "{app}"; Comment: "不限跳数，掉窗口自动等回来；F8 停止"
Name: "{group}\自检（不碰游戏）"; Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; WorkingDir: "{app}"; Comment: "检查依赖 / 模板 / GPU 后端，不发送任何输入"
Name: "{group}\纯 CPU 模式"; Filename: "{app}\jumpbot.exe"; Parameters: "--backend simd --no-cuda --no-opencl --max-jumps 0 --daemon"; WorkingDir: "{app}"
Name: "{group}\只识别不发输入（安全演示）"; Filename: "{app}\jumpbot.exe"; Parameters: "--dry-run --samples 3"; WorkingDir: "{app}"
Name: "{group}\配置文件夹"; Filename: "{app}"
Name: "{group}\卸载 跳一跳自动脚本"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppTitle}"; Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\run.exe"; Parameters: "--finish-config-restore"; Flags: runhidden
Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; Description: "运行自检（推荐，验证依赖与 GPU 后端）"; Flags: postinstall nowait skipifsilent
Filename: "{app}\jumpbot-desktop.exe"; Description: "打开桌面控制台"; Flags: postinstall nowait skipifsilent unchecked

[UninstallRun]
Filename: "{app}\run.exe"; Parameters: "--preserve-uninstall-config"; Flags: runhidden; RunOnceId: "PreserveJumpBotConfig"

[UninstallDelete]
Type: filesandordirs; Name: "{app}\logs"
Type: filesandordirs; Name: "{app}\debug"
