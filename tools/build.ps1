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
      heinrich-settings.exe —— 设置窗口（词组 / 通用 / 学习数据）

    注意 DLL 的文件名是下划线：Cargo 对 cdylib 会把 crate 名里的连字符换成下划线。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build.ps1
#>
[CmdletBinding()]
param(
    [string]$DistDir
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

Write-Host ''
Write-Host "产物：$DistDir" -ForegroundColor Green
# 不递归列出 .stale：那是绕过占用留下的旧文件，不属于产物。
Get-ChildItem -Recurse $DistDir -File |
    Where-Object { $_.FullName -notlike "*$([IO.Path]::DirectorySeparatorChar).stale*" } |
    ForEach-Object {
        Write-Host ("  {0,-24} {1,10:N0} 字节" -f $_.Name, $_.Length)
    }
