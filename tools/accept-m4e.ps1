#requires -Version 5.1
<#
.SYNOPSIS
    M4e 验收：安装包的静默安装 / 静默卸载 / 残留检查。

.DESCRIPTION
    三件事必须一起验，缺一条就等于没验：
      A. 静默安装能把文件放全、把输入法注册好（status 全绿 + 内置 server 自测能跑）
      B. 静默卸载默认**保留**用户数据（%APPDATA%\Heinrich / %LOCALAPPDATA%\Heinrich）
      C. 静默卸载带 /PURGE 才清掉那两个目录

    这台机器上的用户数据是真的（自定义词组、绑的 emoji、学出来的词频），
    所以脚本先把两份目录备份到 %TEMP%，跑完再恢复——purge 那一步是真的删。

    需要管理员：安装包是 PrivilegesRequired=admin，静默安装一样会弹 UAC；
    以普通权限跑脚本会卡在「是否允许此应用更改设备」上等人点。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\accept-m4e.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\accept-m4e.ps1 -SkipRestore
    保留备份不恢复（调试用；备份路径会打印出来）。
#>
[CmdletBinding()]
param(
    [string]$InstallerPath,
    [string]$AppDir,
    [switch]$SkipRestore,
    [switch]$NoRestoreDev
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

# param 默认值里不能用 $PSScriptRoot（-File 调用时参数绑定早于脚本作用域，是空串）。
if (-not $InstallerPath) {
    $found = Get-ChildItem (Join-Path $root 'dist\HeinrichSetup-*.exe') -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $found) {
        Write-Host '找不到 dist\HeinrichSetup-*.exe，先跑：tools\build.ps1 -Installer' -ForegroundColor Red
        exit 2
    }
    $InstallerPath = $found.FullName
}
if (-not $AppDir) { $AppDir = Join-Path $env:ProgramFiles 'HeinrichInput' }

$roaming = Join-Path $env:APPDATA 'Heinrich'
$local = Join-Path $env:LOCALAPPDATA 'Heinrich'
$uninst = Join-Path $AppDir 'unins000.exe'

# AppId 必须与 installer\heinrich.iss 里的 [Setup] AppId 一致（改了那个这里也要改）。
$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{244F2EDA-B5E2-4718-AF0E-CE784E66E244}_is1'
$clsidKey = 'HKLM:\SOFTWARE\Classes\CLSID\{0EB96F60-FFEC-5F48-871F-EA75A989F546}'
$ctfTipKey = 'HKLM:\SOFTWARE\Microsoft\CTF\TIP\{0EB96F60-FFEC-5F48-871F-EA75A989F546}'

$script:failures = 0
function Assert([bool]$Condition, [string]$Message) {
    if ($Condition) {
        Write-Host "  [OK]   $Message" -ForegroundColor Green
    }
    else {
        Write-Host "  [FAIL] $Message" -ForegroundColor Red
        $script:failures++
    }
}

function Write-Step([string]$Title) {
    Write-Host ''
    Write-Host "== $Title ==" -ForegroundColor Cyan
}

function Stop-Server {
    Get-Process -Name 'heinrich-server' -ErrorAction SilentlyContinue |
        ForEach-Object { $_ | Stop-Process -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 300
}

# ---------------------------------------------------------------------------
# [0] 前置
# ---------------------------------------------------------------------------
Write-Step '[0/7] 前置检查'
Write-Host "  安装包 : $InstallerPath"
Write-Host "  安装目录: $AppDir"

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).
    IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host '  需要管理员权限（静默安装同样要写 HKLM）。' -ForegroundColor Red
    Write-Host '  请以管理员身份重开 PowerShell，再跑：' -ForegroundColor Yellow
    Write-Host ('    Start-Process powershell -Verb RunAs -ArgumentList "-ExecutionPolicy","Bypass","-File","{0}"' -f $PSCommandPath) -ForegroundColor DarkGray
    exit 2
}
Assert (Test-Path $InstallerPath) '安装包存在'
if (-not (Test-Path $InstallerPath)) { exit 1 }

# ---------------------------------------------------------------------------
# [1] 备份用户数据
# ---------------------------------------------------------------------------
Write-Step '[1/7] 备份用户数据'
Stop-Server

$backup = Join-Path $env:TEMP ('heinrich-accept-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $backup | Out-Null
$hadRoaming = Test-Path $roaming
$hadLocal = Test-Path $local
if ($hadRoaming) { Copy-Item $roaming (Join-Path $backup 'roaming') -Recurse -Force }
if ($hadLocal) { Copy-Item $local (Join-Path $backup 'local') -Recurse -Force }
Write-Host "  备份到 : $backup"
Write-Host "  roaming: $(if ($hadRoaming) { '已备份' } else { '原本不存在' })"
Write-Host "  local  : $(if ($hadLocal) { '已备份' } else { '原本不存在' })"

# 本机要是从没生成过用户数据（全新机器 / 已清过），造一份最小的测试数据，
# 否则「卸载保留」这一条无从验证——两个目录都不存在，保不保留看起来一样。
if (-not $hadRoaming) {
    New-Item -ItemType Directory -Force -Path $roaming | Out-Null
    Set-Content -Path (Join-Path $roaming 'words.tsv') -Value "验收测试`tvbg" -Encoding UTF8
    Set-Content -Path (Join-Path $roaming 'config.toml') -Value 'version = 1' -Encoding UTF8
}
if (-not $hadLocal) {
    New-Item -ItemType Directory -Force -Path (Join-Path $local 'logs') | Out-Null
    Set-Content -Path (Join-Path $local 'learn.db') -Value 'placeholder' -Encoding UTF8
}

# ---------------------------------------------------------------------------
# [2] 静默安装
# ---------------------------------------------------------------------------
Write-Step '[2/7] 静默安装'
$proc = Start-Process -FilePath $InstallerPath `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCANCEL', '/NOCLOSEAPPLICATIONS' `
    -Wait -PassThru
Assert ($proc.ExitCode -eq 0) "安装器退出码 0（实际 $($proc.ExitCode)）"
Assert (Test-Path (Join-Path $AppDir 'heinrich_win.dll')) 'DLL 已就位'
Assert (Test-Path (Join-Path $AppDir 'heinrich-setup.exe')) '注册器已就位'
Assert (Test-Path (Join-Path $AppDir 'heinrich-settings.exe')) '设置程序已就位'
Assert (Test-Path (Join-Path $AppDir 'data\wubi86\lexicon.tsv')) '五笔词库已就位'
Assert (Test-Path (Join-Path $AppDir 'data\pinyin\lexicon.tsv')) '拼音词库已就位'
Assert (Test-Path (Join-Path $AppDir 'data\wubi86\emoji.toml')) 'emoji 内置表已就位'
Assert (Test-Path $uninst) '卸载器已生成'
Assert (Test-Path $uninstallKey) '控制面板卸载条目已登记'

# ---------------------------------------------------------------------------
# [3] 注册状态与内置数据
# ---------------------------------------------------------------------------
Write-Step '[3/7] 注册状态'
& (Join-Path $AppDir 'heinrich-setup.exe') status | Out-Null
Assert ($LASTEXITCODE -eq 0) 'heinrich-setup status 全绿'

# 安装包里的数据能真装进引擎，才说明 data\ 拷全了（少一个文件 server 起不来）。
& (Join-Path $AppDir 'heinrich-server.exe') --selftest `
    --scheme-dir (Join-Path $AppDir 'data\wubi86') `
    --pinyin-dir (Join-Path $AppDir 'data\pinyin') | Out-Null
Assert ($LASTEXITCODE -eq 0) '安装目录里的 server 自测通过（词库 / 拼音 / emoji 可装载）'

# ---------------------------------------------------------------------------
# [4] 静默卸载（默认保留用户数据）
# ---------------------------------------------------------------------------
Write-Step '[4/7] 静默卸载（默认）'
Stop-Server
# /NOCLOSEAPPLICATIONS：静默模式下 Inno 默认会**直接关掉**占用文件的程序。
# 这里不行——微信和 WorkBuddy 都加载着 heinrich_win.dll，关掉它们等于误伤。
# 代价是删不掉那个 DLL（文件被映射时 Windows 不允许删），见下面的单独断言。
$proc = Start-Process -FilePath $uninst `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCLOSEAPPLICATIONS' `
    -Wait -PassThru
Assert ($proc.ExitCode -eq 0) "卸载器退出码 0（实际 $($proc.ExitCode)）"
$dllLeft = Test-Path (Join-Path $AppDir 'heinrich_win.dll')
$holders = @()
Get-Process -ErrorAction SilentlyContinue | ForEach-Object {
    $p = $_
    try {
        if ($p.Modules | Where-Object { $_.ModuleName -like 'heinrich_win*' }) {
            $holders += "$($p.ProcessName)($($p.Id))"
        }
    }
    catch {}
}
Assert (-not $dllLeft) ('DLL 已删除' + $(if ($dllLeft) { "（被占用：$($holders -join ', ')；Inno 已安排重启后删除）" } else { '' }))
Assert (-not (Test-Path (Join-Path $AppDir 'heinrich-server.exe'))) 'server 已删除'
Assert (-not (Test-Path $clsidKey)) 'CLSID 注册项已删除'
Assert (-not (Test-Path $ctfTipKey)) 'CTF\TIP 注册项已删除'
Assert (-not (Test-Path $uninstallKey)) '控制面板卸载条目已移除'
Assert (Test-Path $roaming) '%APPDATA%\Heinrich 保留（默认不删用户数据）'
Assert (Test-Path $local) '%LOCALAPPDATA%\Heinrich 保留（默认不删学习库）'

# ---------------------------------------------------------------------------
# [5] 重装 → 带 /PURGE 卸载
# ---------------------------------------------------------------------------
Write-Step '[5/7] 重装后带 /PURGE 卸载'
$proc = Start-Process -FilePath $InstallerPath `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/NOCANCEL', '/NOCLOSEAPPLICATIONS' `
    -Wait -PassThru
Assert ($proc.ExitCode -eq 0) '二次安装成功'

Stop-Server
$proc = Start-Process -FilePath $uninst `
    -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/PURGE', '/NOCLOSEAPPLICATIONS' `
    -Wait -PassThru
Assert ($proc.ExitCode -eq 0) '带 /PURGE 卸载退出码 0'
Assert (-not (Test-Path $roaming)) '%APPDATA%\Heinrich 已删除'
Assert (-not (Test-Path $local)) '%LOCALAPPDATA%\Heinrich 已删除'
Assert (-not (Test-Path $clsidKey)) '注册项仍然干净'

# ---------------------------------------------------------------------------
# [6] 恢复
# ---------------------------------------------------------------------------
Write-Step '[6/7] 恢复用户数据'
if ($SkipRestore) {
    Write-Host "  已跳过恢复（-SkipRestore）。备份在：$backup" -ForegroundColor Yellow
}
else {
    if (Test-Path $roaming) { Remove-Item $roaming -Recurse -Force }
    if (Test-Path $local) { Remove-Item $local -Recurse -Force }
    if ($hadRoaming) { Copy-Item (Join-Path $backup 'roaming') $roaming -Recurse -Force }
    if ($hadLocal) { Copy-Item (Join-Path $backup 'local') $local -Recurse -Force }
    Assert ((Test-Path $roaming) -eq $hadRoaming) 'roaming 已按原样恢复'
    Assert ((Test-Path $local) -eq $hadLocal) 'local 已按原样恢复'
    if ((-not $hadRoaming) -and (-not $hadLocal)) {
        Write-Host '  本机原本没有用户数据，测试数据已清理' -ForegroundColor DarkGray
    }
    Remove-Item $backup -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host '== [7/7] 恢复开发态注册 ==' -ForegroundColor Cyan
# 本脚本把注册表从中指 dist 的「开发态」改成了 Program Files，卸载后又清空。
# 不恢复的话，用户跑完验收会发现输入法列表里没有「亨利五笔」了。
# 恢复动作就是 dist 里那套 setup install（同一个注册器，同一套参数）。
$devSetup = Join-Path $root 'dist\heinrich-setup.exe'
if ($NoRestoreDev) {
    Write-Host '  已跳过（-NoRestoreDev）。要恢复请手动跑：dist\heinrich-setup.exe install' -ForegroundColor Yellow
}
elseif (-not (Test-Path $devSetup)) {
    Write-Host "  找不到 $devSetup，跳过恢复" -ForegroundColor Yellow
}
else {
    & $devSetup install | Out-Null
    Assert ($LASTEXITCODE -eq 0) '开发态注册已恢复（dist 目录）'
    Assert (Test-Path $clsidKey) 'CLSID 注册项回来了'
}

Write-Host ''
if ($script:failures -eq 0) {
    Write-Host '全部通过。' -ForegroundColor Green
    Write-Host '提示：卸载/重装会动输入法列表，任务栏和已打开的程序可能需要切一次输入法。' -ForegroundColor DarkGray
    exit 0
}
Write-Host "有 $($script:failures) 项未通过。" -ForegroundColor Red
exit 1
