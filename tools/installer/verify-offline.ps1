# Validate the standalone preview only, under the current account. No game is started.
param([Parameter(Mandatory=$true)][string]$Installer, [string]$PreviousInstaller)
$ErrorActionPreference = 'Stop'
$installerPath = (Resolve-Path -LiteralPath $Installer).Path
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path
$id = '{AA105DEB-419A-4E5B-A840-4F7E2332A61C}_is1'
$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$id"
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
$desktopLink = Join-Path $desktop 'JumpBot 控制台.lnk'
$oldDesktopLink = Join-Path $desktop '跳一跳控制台（离线测试版）.lnk'
if ((Test-Path -LiteralPath $key) -or (Test-Path -LiteralPath $desktopLink) -or (Test-Path -LiteralPath $oldDesktopLink) -or
    (Get-Process -Name jumpbot,jumpbot-desktop -ErrorAction SilentlyContinue)) {
    throw '此账户已有离线测试版或同名快捷方式。请使用空闲测试账户。'
}
$nonce = [guid]::NewGuid().ToString('N').Substring(0,10)
$proof = Join-Path $repo "build\offline-install-proof-$nonce"
$app = Join-Path $proof '中文 自选目录'
$groupName = "JumpBot-Offline-QA-$nonce"
$group = Join-Path ([Environment]::GetFolderPath('Programs')) $groupName
$frame = Join-Path $repo 'tests\fixtures\skin-gold-play.png'
$null = New-Item -ItemType Directory -Path $proof
function Check($ok, [string]$message) {
    if (!$ok) { throw $message }
}
function Hash([string]$file) {
    $path = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($file)
    $stream = [IO.File]::OpenRead($path)
    try {
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try {
            [BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '')
        } finally { $sha256.Dispose() }
    } finally { $stream.Dispose() }
}
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

public static class ShortcutFileIdentity
{
    [StructLayout(LayoutKind.Sequential)]
    private struct FileIdInfo
    {
        public ulong VolumeSerialNumber, IdLow, IdHigh;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandleEx(SafeFileHandle handle,
        int infoClass, out FileIdInfo info, uint size);

    public static string Read(string path)
    {
        using (var file = File.OpenRead(path))
        {
            FileIdInfo id;
            const int FileIdInfoClass = 18;
            if (!GetFileInformationByHandleEx(file.SafeFileHandle, FileIdInfoClass, out id,
                    (uint)Marshal.SizeOf(typeof(FileIdInfo))))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            return String.Format("{0:X16}:{1:X16}{2:X16}", id.VolumeSerialNumber, id.IdHigh, id.IdLow);
        }
    }
}
'@
$shell = New-Object -ComObject Shell.Application
function CheckShortcut([string]$lnk, [bool]$selected, [string]$target, [string]$arguments) {
    $exists = Test-Path -LiteralPath $lnk -PathType Leaf
    Check ($exists -eq $selected) "快捷方式存在性错误 [$lnk] 实际=[$exists] 期望=[$selected]"
    if (!$selected) { return }

    $actualTarget = $actualArgs = $actualId = $expectedId = '<未读取>'
    try {
        $item = $shell.NameSpace([IO.Path]::GetDirectoryName($lnk)).ParseName([IO.Path]::GetFileName($lnk))
        $link = $item.GetLink
        if ($null -eq $link) { throw 'Shell 无法读取快捷方式' }
        $actualTarget = [string]$link.Path
        $actualArgs = [string]$link.Arguments
        $expectedId = [ShortcutFileIdentity]::Read($target)
        $actualId = [ShortcutFileIdentity]::Read($actualTarget)
        Check ($actualId -ceq $expectedId -and [string]::Equals($actualArgs, $arguments, [StringComparison]::Ordinal)) '目标文件或参数不同'
    } catch {
        Copy-Item -LiteralPath $lnk -Destination $proof -Force
        throw "快捷方式校验失败 [$lnk] $($_.Exception.Message)`n实际目标=[$actualTarget] 参数=[$actualArgs] 文件标识=[$actualId]`n期望目标=[$target] 参数=[$arguments] 文件标识=[$expectedId]`n快捷方式副本位于 [$proof]"
    }
}
function Setup([string]$components, [bool]$shortcut, [string]$directory, [string]$label, [string]$openCv, [string]$binary = $installerPath, [string]$menu = $groupName) {
    $task = if ($shortcut) {'desktopicon'} else {'!desktopicon'}
    $params = @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/CURRENTUSER',
                "/DIR=""$directory""","/COMPONENTS=""$components""",
                "/MERGETASKS=$task","/LOG=""$proof\$label.log""")
    if ($menu) { $params += "/GROUP=$menu" }
    if ($openCv) { $params += "/OPENCV=""$openCv""" }
    (Start-Process -FilePath $binary -ArgumentList $params -Wait -PassThru -WindowStyle Hidden).ExitCode
}
function RemovePreview {
    $unins = Join-Path $app 'unins000.exe'
    Check ((Get-ItemProperty -LiteralPath $key).'Inno Setup: App Path' -eq $app) '测试版注册路径与本次目录不同'
    $exit = (Start-Process -FilePath $unins -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART' -Wait -PassThru -WindowStyle Hidden).ExitCode
    Check ($exit -eq 0) '卸载失败'
    $until = [DateTime]::UtcNow.AddSeconds(30)
    while ((Test-Path -LiteralPath $unins) -and [DateTime]::UtcNow -lt $until) {Start-Sleep -Milliseconds 100}
    foreach ($p in @($unins,$key,$desktopLink,(Join-Path $app 'jumpbot.exe'),(Join-Path $app 'jumpbot-desktop.exe'))) {
        Check (!(Test-Path -LiteralPath $p)) "卸载残留 $p"
    }
}
function Detect([string]$backend) {
    $log = Join-Path $proof "$choice-$backend-$env:JUMPBOT_DISABLE_CUDA"
    $proc = Start-Process -FilePath (Join-Path $app 'jumpbot.exe') -WorkingDirectory $app -ArgumentList "--backend $backend --image ""$frame""" -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput "$log.json" -RedirectStandardError "$log.stderr"
    Check ($proc.ExitCode -eq 0) "识别失败 $backend"
    $r = Get-Content -LiteralPath "$log.json" -Raw -Encoding UTF8 | ConvertFrom-Json
    Check ($null -ne $r.piece -and $null -ne $r.target -and $r.backend -in @('cpu','simd','cuda','opencl')) '没有识别到棋子和目标'
    return $r
}
$env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
$env:JUMPBOT_NO_INPUT = '1'
$env:JUMPBOT_DISABLE_CUDA = '0'
$env:JUMPBOT_DISABLE_OPENCL = '0'
$rows = @()
$available = @{}
$reference = $null
$userConfig = $null
$userDigit = $null
$cfg = Join-Path $app 'config.json'
$digit = Join-Path $app 'templates\digit_0.png'
try {
    # A non-empty foreign directory must be rejected even with a silent /DIR override.
    $foreign = New-Item -ItemType Directory (Join-Path $proof 'existing')
    [IO.File]::WriteAllText((Join-Path $foreign 'keep.txt'),'keep')
    Check ((Setup 'core' $false $foreign.FullName 'foreign') -ne 0) '安装器接受了已有非测试目录'
    Check (@(Get-ChildItem -LiteralPath $foreign -Force).Count -eq 1) '已有目录被写入'
    Check ((Setup 'core' $false $app 'missing-cv' (Join-Path $proof 'missing.dll')) -ne 0) '缺失 DLL 被接受'
    Check (!(Test-Path -LiteralPath $key) -and !(Test-Path -LiteralPath (Join-Path $app 'jumpbot.exe'))) '无效的 DLL 来源启动了安装'
    if ($PreviousInstaller) {
        $previous = (Resolve-Path -LiteralPath $PreviousInstaller).Path
        Check ((Get-Item -LiteralPath $previous).VersionInfo.FileDescription -ceq '跳一跳控制台（离线测试版） 安装程序') '旧文件与预览安装器不符'
        $programs = [Environment]::GetFolderPath('Programs')
        $legacyGroup = Join-Path $programs '跳一跳控制台（离线测试版）'
        foreach ($dir in @($legacyGroup, (Join-Path $programs 'JumpBot'))) {
            Check (!(Test-Path -LiteralPath $dir)) "测试菜单已存在，请使用空闲账户：$dir"
        }
        $gui = Join-Path $app 'jumpbot-desktop.exe'
        foreach ($oldMenu in @('', $groupName)) {
            $expected = if ($oldMenu) {$oldMenu} else {'JumpBot'}
            Check ((Setup 'core' $true $app "preview-2-$expected" '' $previous $oldMenu) -eq 0) '旧预览安装失败'
            $reg = Get-ItemProperty -LiteralPath $key
            Check ($reg.DisplayVersion -match 'preview\.2$' -and $reg.'Inno Setup: App Path' -eq $app) '旧安装器不是预期的 preview.2'
            CheckShortcut $oldDesktopLink $true $gui ''
            if (!$oldMenu) {
                Check ($reg.'Inno Setup: Icon Group' -ceq '跳一跳控制台（离线测试版）') '未使用真实旧默认菜单组'
                $foreignMenuLink = Join-Path $legacyGroup 'OpenCL 模式.lnk'
                [IO.File]::Copy((Join-Path $legacyGroup '自检（不碰游戏）.lnk'), $foreignMenuLink, $false)
            }
            Check ((Setup 'core' $true $app "upgrade-$expected" '' $installerPath '') -eq 0) '原目录升级失败'
            $reg = Get-ItemProperty -LiteralPath $key
            Check ($reg.DisplayName -ceq '跳一跳控制台' -and $reg.'Inno Setup: App Path' -eq $app -and $reg.'Inno Setup: Icon Group' -ceq $expected) '改名后登记或菜单组错误'
            CheckShortcut $oldDesktopLink $false $gui ''
            CheckShortcut $desktopLink $true $gui ''
            CheckShortcut (Join-Path (Join-Path $programs $expected) '跳一跳控制台.lnk') $true $gui ''
            if (!$oldMenu) {
                CheckShortcut (Join-Path $legacyGroup '跳一跳控制台.lnk') $false $gui ''
                CheckShortcut $foreignMenuLink $true (Join-Path $app 'jumpbot.exe') '--selftest'
                Remove-Item -LiteralPath $foreignMenuLink
                [IO.Directory]::Delete($legacyGroup)
            }
            RemovePreview
        }
    }
    $localCv = Join-Path $proof '本机 已有.dll'
    foreach ($choice in @('cpu','both','cuda','opencl','cpu-again')) {
        $cuda = $choice -in @('cuda','both')
        $cl = $choice -in @('opencl','both')
        $components = (@('core'; if ($cuda) {'cuda'}; if ($cl) {'opencl'})) -join ','
        $cvChoice = switch ($choice) { 'both' {$localCv} 'cuda' {'installed'} 'opencl' {'bundled'} }
        Check ((Setup $components $cuda $app $choice $cvChoice) -eq 0) "安装失败 $choice"
        Check ((Get-ItemProperty -LiteralPath $key).'Inno Setup: App Path' -eq $app) '注册目录错误'
        $manifest = Get-Content -LiteralPath (Join-Path $app 'build-source.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        $workerKey = switch ($choice) {both {'jumpbot.exe'} 'cpu-again' {'workers/cpu/jumpbot.exe'} default {"workers/$choice/jumpbot.exe"}}
        $actualHash = Hash (Join-Path $app 'jumpbot.exe')
        Check ($actualHash -eq $manifest.files_sha256.$workerKey) "安装的工作程序不符合组件 $choice"
        foreach ($item in $manifest.files_sha256.PSObject.Properties) {
            if ($item.Name -match '^(jumpbot\.exe$|workers/|config\.json$|templates/)') {continue}
            Check ((Hash (Join-Path $app $item.Name)) -eq $item.Value) "缺少或不符 $($item.Name)"
        }
        foreach ($dll in @('vcruntime140.dll','msvcp140.dll')) {Check (Test-Path (Join-Path $app $dll)) "缺少 $dll"}
        $cv = @(Get-ChildItem -LiteralPath $app -Filter 'opencv_world*.dll')
        Check ($cv.Count -eq 1) '缺少唯一 OpenCV 运行库'
        $entry = if ($choice -in @('both','cuda')) {'local'} else {'bundled'}
        $log = Get-Content -LiteralPath (Join-Path $proof "$choice.log") -Raw -Encoding UTF8
        $entries = [regex]::Matches($log,'OpenCV file entry: (local|bundled)')
        Check ($entries.Count -eq 1 -and $entries[0].Groups[1].Value -eq $entry) '安装日志中的实际 OpenCV 文件来源错误'
        if ($entry -eq 'local') {
            $expectedSource = if ($choice -eq 'both') {$localCv} else {$cv[0].FullName}
            Check ($log.Contains("OpenCV source: local | $expectedSource")) '实际校验的来源路径错误'
        }
        if ($choice -eq 'cpu') {
            Copy-Item -LiteralPath $cv[0].FullName -Destination $localCv
            $bad = Join-Path $proof 'same-size-wrong.dll'
            $bytes = [IO.File]::ReadAllBytes($localCv)
            $bytes[0] = $bytes[0] -bxor 1
            [IO.File]::WriteAllBytes($bad, $bytes)
            Check ((Setup 'core,cuda' $false $app 'bad-cv' $bad) -ne 0) '相同长度但哈希不同的 DLL 被接受'
            Check ((Hash (Join-Path $app 'jumpbot.exe')) -eq $actualHash -and (Hash $cv[0].FullName) -eq (Hash $localCv)) '拒绝错误来源时改变了已安装程序'
        }
        CheckShortcut $desktopLink $cuda (Join-Path $app 'jumpbot-desktop.exe') ''
        foreach ($link in @(@('NVIDIA CUDA 模式','cuda',$cuda),@('OpenCL 模式','opencl',$cl))) {
            CheckShortcut (Join-Path $group ($link[0]+'.lnk')) $link[2] (Join-Path $app 'jumpbot.exe') "--backend $($link[1]) --max-jumps 0 --daemon"
        }
        if ($null -eq $reference) {
            $reference = Detect 'cpu'
            [IO.File]::WriteAllText($cfg,'{"k":2.83,"gamma":0}')
            Copy-Item (Join-Path $app 'templates\digit_2.png') $digit -Force
            $userConfig = Hash $cfg
            $userDigit = Hash $digit
        }
        Check ((Hash $cfg) -eq $userConfig -and (Hash $digit) -eq $userDigit) '升级覆盖了用户配置或模板'
        foreach ($backend in @('cuda','opencl','auto')) {
            $row = Detect $backend
            Check ($row.piece_mask_crc32 -eq $reference.piece_mask_crc32 -and $row.block_mask_crc32 -eq $reference.block_mask_crc32) '后端图像结果不一致'
            Check (($row.backend -ne 'cuda' -or $cuda) -and ($row.backend -ne 'opencl' -or $cl)) '使用了未安装的加速组件'
            if ($choice -eq 'both') { $available[$backend] = $row.backend }
            if ($choice -eq $backend -and $available[$backend] -eq $backend) {
                Check ($row.backend -eq $backend) "单选组件未能使用已验证的 $backend"
            }
            $rows += [pscustomobject]@{components=$components; requested=$backend; actual=$row.backend; worker=$actualHash}
        }
        if ($choice -eq 'both') {
            $env:JUMPBOT_DISABLE_CUDA='1'
            $nonNvidia = Detect 'opencl'
            Check ($nonNvidia.backend -eq $available['opencl'] -and $nonNvidia.piece_mask_crc32 -eq $reference.piece_mask_crc32) '禁用 CUDA 后 OpenCL 路径异常'
            $rows += [pscustomobject]@{components=$components;requested='opencl (CUDA disabled)';actual=$nonNvidia.backend;worker=$actualHash}
            $env:JUMPBOT_DISABLE_OPENCL='1'
            $fallback = Detect 'auto'
            Check ($fallback.backend -in @('simd','cpu') -and $fallback.piece_mask_crc32 -eq $reference.piece_mask_crc32 -and $fallback.block_mask_crc32 -eq $reference.block_mask_crc32) '屏蔽显卡后 CPU 回退失败'
            $rows += [pscustomobject]@{components=$components;requested='auto (both drivers disabled)';actual=$fallback.backend;worker=$actualHash}
            $env:JUMPBOT_DISABLE_CUDA='0'
            $env:JUMPBOT_DISABLE_OPENCL='0'
        }
    }
    RemovePreview
    Check ((Hash $localCv) -eq $manifest.files_sha256.($cv[0].Name)) '原始 DLL 未获保留'
    Check ((Hash (Join-Path $app 'config.jumpbot-preserved.json')) -eq $userConfig -and (Hash $digit) -eq $userDigit) '卸载丢失配置或模板'
    Check ((Setup 'core' $false $app 'reinstall') -eq 0) '重新安装失败'
    Check ((Hash $cfg) -eq $userConfig -and (Hash $digit) -eq $userDigit -and !(Test-Path (Join-Path $app 'config.jumpbot-preserved.json'))) '重装未还原用户数据'
    RemovePreview
    $rows | ConvertTo-Json | Set-Content (Join-Path $proof 'backends.json') -Encoding UTF8
    Write-Host "安装、组件切换、回退、卸载、重装验证通过。记录在 $proof"
} finally {
    if ((Test-Path -LiteralPath $key) -and ((Get-ItemProperty -LiteralPath $key).'Inno Setup: App Path' -eq $app)) { RemovePreview }
}
