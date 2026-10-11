; 跳一跳自动脚本 —— Inno Setup 安装包脚本
;
; 编译（由 tools/installer/build_installer.ps1 调用）：
;   ISCC.exe /DPayloadDir="<dist\jumpbot 绝对路径>" /DOutputDir="<输出目录>" jumpbot.iss
;
; 设计要点
;  * 每用户安装（PrivilegesRequired=lowest，不弹 UAC），装到 %LOCALAPPDATA%\Programs\jumpbot。
;    bot 会把 logs/ debug/ 写在 exe 旁边，所以安装目录必须用户可写——Program Files 不合适。
;  * 只装运行必需的东西：exe + _internal + 配置 + 启动器 + README。
;    **排除 logs/ 与 debug/**：那是跑起来之后的运行数据（实测混进去能到 49MB）。
;  * 快捷方式直接指向 exe 带参数（不走 .bat，避免每次闪一个 cmd 窗口）。
;  * 带标准卸载程序，并登记到「应用和功能」里。

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
UninstallDisplayIcon={app}\jumpbot.exe
SetupLogging=yes
AllowNoIcons=yes

[Languages]
Name: "chinese"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加任务："; Flags: checkedonce

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; \
    Excludes: "\logs\*,\debug\*,\logs,\debug,\_internal\logs\*,\_internal\debug\*,\_internal\logs,\_internal\debug,\config.json,\README.md"; \
    Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\config.json"; DestDir: "{app}"; Flags: onlyifdoesntexist

[Icons]
Name: "{group}\跳一跳自动脚本（长跑）"; Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; WorkingDir: "{app}"; Comment: "不限跳数，掉窗口自动等回来；F8 停止"
Name: "{group}\自检（不碰游戏）"; Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; WorkingDir: "{app}"; Comment: "检查依赖 / 模板 / GPU 后端，不发送任何输入"
Name: "{group}\纯 CPU 模式"; Filename: "{app}\jumpbot.exe"; Parameters: "--backend simd --no-cuda --no-opencl --max-jumps 0 --daemon"; WorkingDir: "{app}"
Name: "{group}\只识别不发输入（安全演示）"; Filename: "{app}\jumpbot.exe"; Parameters: "--dry-run --samples 3"; WorkingDir: "{app}"
Name: "{group}\配置文件夹"; Filename: "{app}"
Name: "{group}\卸载 跳一跳自动脚本"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppTitle}"; Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\jumpbot.exe"; Parameters: "--selftest"; Description: "运行自检（推荐，验证依赖与 GPU 后端）"; Flags: postinstall nowait skipifsilent
Filename: "{app}\jumpbot.exe"; Parameters: "--max-jumps 0 --daemon"; Description: "立即启动长跑（请先在微信里打开「跳一跳」并保持窗口可见）"; Flags: postinstall nowait skipifsilent unchecked

[UninstallDelete]
Type: filesandordirs; Name: "{app}\logs"
Type: filesandordirs; Name: "{app}\debug"
