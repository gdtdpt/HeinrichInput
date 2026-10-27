#requires -Version 5.1
<#
.SYNOPSIS
    构建 release 产物并归集到 dist/。

.DESCRIPTION
    当前（M2.2）产物：
      heinrich-cli.exe    —— 引擎 REPL 与方案校验
      heinrich-gen.exe    —— 数据装配（发版时不需要随包）
      heinrich-setup.exe  —— TSF 注册器
      heinrich_win.dll    —— TSF 前端（必须与 setup 同目录）
      heinrich-server.exe —— 常驻引擎宿主（必须与 DLL 同目录：DLL 靠「同目录」找它）
      heinrich-cand-preview.exe —— 候选窗预览器（验收工具，发版时不需要随包）
      heinrich-settings.exe —— 设置窗口（通用 / 词组 / emoji / 学习数据）

    注意 DLL 的文件名是下划线：Cargo 对 cdylib 会把 crate 名里的连字符换成下划线。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build.ps1 -Installer
    顺带用 Inno Setup 打出 dist\HeinrichSetup-<版本>.exe（需要构建机装 ISCC）。
#>
[CmdletBinding()]
param(
    [string]$DistDir,
    [switch]$Installer
)

# 只对 PowerShell 自己的 cmdlet 用 Stop。原生程序写 stderr 时，Windows PowerShell 5.1
# 会把那些行包成 NativeCommandError，配合 Stop 直接终止脚本——而 cargo 的进度、
# 编译警告全都走 stderr，于是「构建其实成功了但脚本在这一行挂掉」。
# 原生命令的成败一律用 $LASTEXITCODE 判断，所以调用期间临时放开 Stop。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串。
if (-not $DistDir) { $DistDir = Join-Path $PSScriptRoot '..\dist' }

# 显式按包名列出本平台的产物，不用 --workspace / --all。
# M5 加入 apps/macos、apps/linux 之后，--workspace 会去编别的平台的壳、必然失败；
# 现在就这么写，届时不用户回头改（PROJECT.md D24）。
$ErrorActionPreference = 'Continue'
& cargo build --release -p heinrich-cli -p heinrich-gen -p heinrich-win -p heinrich-setup -p heinrich-server -p heinrich-cand-preview -p heinrich-settings 2>&1 | ForEach-Object { Write-Host $_ }
$buildExit = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
if ($buildExit -ne 0) {
    Write-Host '构建失败' -ForegroundColor Red
    exit $buildExit
}
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

# 常驻的 server 会锁住自己的 exe（和 DLL 被宿主映射是同一类问题），但它是我方进程、
# 没有用户数据，直接结束即可：DLL 下一次按键会自动重新拉起新版。
Get-Process -Name 'heinrich-server' -ErrorAction SilentlyContinue |
    ForEach-Object {
        Write-Host "  结束正在运行的 heinrich-server（pid $($_.Id)）" -ForegroundColor Yellow
        $_ | Stop-Process -Force -ErrorAction SilentlyContinue
    }
Start-Sleep -Milliseconds 200

foreach ($name in @('heinrich-cli.exe', 'heinrich-gen.exe', 'heinrich-setup.exe', 'heinrich_win.dll', 'heinrich-server.exe', 'heinrich-cand-preview.exe', 'heinrich-settings.exe')) {
    $src = Join-Path $root "target\release\$name"
    if (-not (Test-Path $src)) {
        Write-Host "  缺少 $name" -ForegroundColor Red
        exit 1
    }
    try {
        Copy-Item $src -Destination $DistDir -Force -ErrorAction Stop
        Write-Host "  已复制 $name"
        continue
    }
    catch {
        # 已注册的 DLL 会被宿主进程映射，Windows 锁住镜像文件：覆盖必然失败。
        # 且 COM 的 DLL 要等**宿主进程退出**才卸载，切换输入法、切走窗口都不够。
        # 开发期最容易踩的是宿主里包含自己的终端：在 WorkBuddy 内置终端里试过一次
        # 输入法之后，WorkBuddy 就一直持有这个 DLL。
        if ($name -ne 'heinrich_win.dll') {
            Write-Host "  无法写入 $name" -ForegroundColor Red
            Write-Host "  $($_.Exception.Message)" -ForegroundColor DarkGray
            exit 1
        }
    }

    # 绕过办法：Windows 允许**重命名**正在映射的镜像文件（只是不允许删除或覆盖）。
    # 把旧的挪进 dist\.stale\，新文件就能落位：运行中的宿主继续映射旧内容、不受影响，
    # 之后新启动的宿主会加载新版。代价是 .stale 里会留旧文件，可以随时整个删掉。
    $stale = Join-Path $DistDir '.stale'
    New-Item -ItemType Directory -Force -Path $stale | Out-Null
    $dst = Join-Path $DistDir $name
    $bak = Join-Path $stale ('heinrich_win.' + (Get-Date -Format 'MMdd-HHmmss') + '.dll')
    try {
        Move-Item -Path $dst -Destination $bak -ErrorAction Stop
        Copy-Item $src -Destination $dst -Force -ErrorAction Stop
        Write-Host "  已复制 $name（旧文件被占用，已改名到 .stale\ 绕过）" -ForegroundColor Yellow
    }
    catch {
        Write-Host "  无法写入 $name，改名绕过也失败" -ForegroundColor Red
        Write-Host "  $($_.Exception.Message)" -ForegroundColor DarkGray
        Write-Host ''
        Write-Host '  这个 DLL 正被已加载它的程序占用。请关闭这些程序后再构建：' -ForegroundColor Yellow
        Write-Host '    记事本 / Notepad++ / VS Code / Chrome / Electron 应用（含 WorkBuddy）/ 资源管理器'
        Write-Host '  查哪个进程持有它（可选）：' -ForegroundColor DarkGray
        Write-Host '    Get-Process | Where-Object { $_.Modules.FileName -like "*heinrich_win.dll" }' -ForegroundColor DarkGray
        exit 1
    }
}

# 方案数据是运行时依赖，与可执行同层放置，目录结构保持一致
$dataOut = Join-Path $DistDir 'data\wubi86'
New-Item -ItemType Directory -Force -Path $dataOut | Out-Null
Copy-Item (Join-Path $root 'data\wubi86\*') -Destination $dataOut -Force

# 词组 emoji 内置表（M4c / D66）随 `data\wubi86\*` 一起拷过去了，但缺了它只降级
# （候选照常出，只是不戴 emoji）——这种「静默少了个功能」的降级必须在构建时吼一声。
$emojiSrc = Join-Path $root 'data\wubi86\emoji.toml'
if (-not (Test-Path $emojiSrc)) {
    Write-Host '  未找到 data\wubi86\emoji.toml，dist 里将没有 emoji（候选不带 emoji）' -ForegroundColor Yellow
    Write-Host '  需要 emoji 请先跑：powershell -ExecutionPolicy Bypass -File tools\gen.ps1 -Kind emoji' -ForegroundColor DarkGray
}

# 拼音数据（M3 混输）。**可选**：server 找不到这一份就按纯五笔跑，
# 所以缺了只警告、不中断构建——但它确实是发版该带的东西，得吼一声。
$pinyinSrc = Join-Path $root 'data\pinyin'
if (Test-Path $pinyinSrc) {
    $pinyinOut = Join-Path $DistDir 'data\pinyin'
    New-Item -ItemType Directory -Force -Path $pinyinOut | Out-Null
    Copy-Item (Join-Path $pinyinSrc '*') -Destination $pinyinOut -Force
}
else {
    Write-Host '  未找到 data\pinyin，dist 里将没有拼音数据（server 会按纯五笔运行）' -ForegroundColor Yellow
    Write-Host '  需要混输请先跑：powershell -ExecutionPolicy Bypass -File tools\gen.ps1 -Kind pinyin' -ForegroundColor DarkGray
}

# 语言列表图标（D72）：注册表 IconFile 指向它。缺了就退回把 DLL 当图标源
# （DLL 里没有图标资源 ⇒ 输入法列表里一块空白），所以这里必须带上。
$iconSrc = Join-Path $root 'data\icon\heinrich.ico'
if (Test-Path $iconSrc) {
    Copy-Item $iconSrc -Destination $DistDir -Force
}
else {
    Write-Host '  未找到 data\icon\heinrich.ico，输入法列表里将没有图标' -ForegroundColor Yellow
    Write-Host '  需要图标请先跑：powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1' -ForegroundColor DarkGray
}

Write-Host ''
Write-Host "产物：$DistDir" -ForegroundColor Green
# 不递归列出 .stale：那是绕过占用留下的旧文件，不属于产物。
Get-ChildItem -Recurse $DistDir -File |
    Where-Object { $_.FullName -notlike "*$([IO.Path]::DirectorySeparatorChar).stale*" } |
    ForEach-Object {
        Write-Host ("  {0,-24} {1,10:N0} 字节" -f $_.Name, $_.Length)
    }

# ---------------------------------------------------------------------------
# 安装包（M4e / D93）
# ---------------------------------------------------------------------------
# ISCC 只在**构建机**上需要：产物是单 exe 自解压，内网脱机分发不依赖任何运行时。
# 没装就跳过——不加 -Installer 的日常构建完全不受影响。
if (-not $Installer) {
    exit 0
}

Write-Host ''
Write-Host '== 安装包 ==' -ForegroundColor Green

function Find-Iscc {
    # 顺序：环境变量 → winget 的用户级安装 → 官方安装器的两个默认位置 → PATH。
    # 不用 Join-Path 直接拼可能为空的 env（${env:ProgramFiles(x86)} 在 32 位系统上是空的，
    # Join-Path 会抛「Path 为空」）。
    $candidates = @()
    if ($env:ISCC) { $candidates += $env:ISCC }
    if ($env:LOCALAPPDATA) { $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe') }
    foreach ($base in @(${env:ProgramFiles(x86)}, $env:ProgramFiles)) {
        if ($base) { $candidates += (Join-Path $base 'Inno Setup 6\ISCC.exe') }
    }
    foreach ($c in $candidates) {
        if (Test-Path $c) { return $c }
    }
    $cmd = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

$iscc = Find-Iscc
if (-not $iscc) {
    Write-Host '  找不到 ISCC.exe（Inno Setup 6 的编译器）' -ForegroundColor Red
    Write-Host '  安装（任选一种）：' -ForegroundColor Yellow
    Write-Host '    winget install --id JRSoftware.InnoSetup --source winget' -ForegroundColor DarkGray
    Write-Host '    https://jrsoftware.org/isdl.php' -ForegroundColor DarkGray
    Write-Host '  也可以用环境变量 ISCC 指向它，构建本身已经成功，只是没有安装包。' -ForegroundColor DarkGray
    exit 1
}

# 版本号与 Cargo 同源：安装包、控制面板、heinrich-setup status 显示同一个数。
$cargoToml = Get-Content (Join-Path $root 'Cargo.toml') -Raw
$verMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $verMatch.Success) {
    Write-Host '  无法从 Cargo.toml 读出 workspace 版本号' -ForegroundColor Red
    exit 1
}
$appVersion = $verMatch.Groups[1].Value
Write-Host "  ISCC     : $iscc"
Write-Host "  版本     : $appVersion"

& $iscc "/DAppVersion=$appVersion" (Join-Path $root 'installer\heinrich.iss') |
    ForEach-Object { Write-Host "  $_" }
if ($LASTEXITCODE -ne 0) {
    Write-Host '  安装包编译失败' -ForegroundColor Red
    exit $LASTEXITCODE
}

$setupName = "HeinrichSetup-$appVersion.exe"
$setupPath = Join-Path $DistDir $setupName
if (-not (Test-Path $setupPath)) {
    Write-Host "  编译报告成功，但没找到 $setupPath" -ForegroundColor Red
    exit 1
}
$size = (Get-Item $setupPath).Length
Write-Host ("  安装包   : {0}（{1:N1} MB）" -f $setupPath, ($size / 1MB)) -ForegroundColor Green

