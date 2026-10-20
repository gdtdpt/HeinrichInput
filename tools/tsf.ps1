#requires -Version 5.1
<#
.SYNOPSIS
    注册 / 反注册 / 检查「亨利五笔」输入法。

.DESCRIPTION
    heinrich-setup.exe 的 PowerShell 入口。做的事和直接跑 exe 一样，
    额外把三条命令的用法和「装完该看哪里」串起来。

    install / uninstall 需要写 HKLM，会弹 UAC。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 status
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 install

.EXAMPLE
    # 直接看日志尾部
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log

.EXAMPLE
    # 打开逐键 debug 日志（改完要重启宿主程序才生效）
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug off
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('install', 'uninstall', 'status', 'log', 'debug')]
    [string]$Action = 'status',

    # 只给 debug 用：`tsf.ps1 debug on` / `tsf.ps1 debug off`。
    # 位置必须显式声明：PowerShell 按声明顺序分配位置参数，不写 Position 的话
    # `debug on` 里的 `on` 会落到下一个按顺序排列的参数上。
    [Parameter(Position = 1)]
    [ValidateSet('on', 'off', '')]
    [string]$Switch = '',

    [Parameter(Position = 2)]
    [string]$DistDir,

    [Parameter(Position = 3)]
    [int]$TailLines = 40
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串，
# `Join-Path` 会直接抛「无法将参数绑定到参数 Path，因为该参数为空字符串」。
if (-not $DistDir) { $DistDir = Join-Path $PSScriptRoot '..\dist' }

$setup = Join-Path $DistDir 'heinrich-setup.exe'
$logDir = Join-Path $env:LOCALAPPDATA 'Heinrich'
$logPath = Join-Path $logDir 'logs\tsf.log'
$debugMarker = Join-Path $logDir 'debug.on'

function Test-Elevated {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Show-Log {
    if (-not (Test-Path $logPath)) {
        Write-Host "日志还没有生成：$logPath" -ForegroundColor Yellow
        Write-Host '先切到「亨利五笔」在记事本里打几个字母，再回来看。'
        return
    }
    $info = Get-Item $logPath
    Write-Host ("== 日志 {0}（{1:N0} 字节，最后写入 {2:HH:mm:ss}）" -f $logPath, $info.Length, $info.LastWriteTime) -ForegroundColor Cyan
    # 必须显式 UTF8：Windows PowerShell 5.1 的 Get-Content 默认按 ANSI(936) 解码，
    # 而 DLL 写的是 UTF-8，不指定编码时中文会变成「鈥斺€?鎸夐敭」。
    Get-Content $logPath -Tail $TailLines -Encoding UTF8
}

function Set-DebugSwitch {
    param([string]$Value)
    switch ($Value) {
        'on' {
            New-Item -ItemType Directory -Force -Path $logDir | Out-Null
            Set-Content -Path $debugMarker -Value 'debug' -Encoding ASCII
            Write-Host "已打开 debug 日志：$debugMarker" -ForegroundColor Green
            Write-Host '注意：日志级别在每个宿主进程里只解析一次，' -ForegroundColor Yellow
            Write-Host '      必须**重启要观察的程序**（如 Notepad++）才生效。'
        }
        'off' {
            if (Test-Path $debugMarker) { Remove-Item $debugMarker -Force }
            Write-Host '已关闭 debug 日志，回到 info 级。' -ForegroundColor Green
            Write-Host '同样要重启宿主程序才生效。' -ForegroundColor Yellow
        }
        default {
            if (Test-Path $debugMarker) {
                Write-Host "debug 日志：已打开（$debugMarker）" -ForegroundColor Green
            }
            else {
                Write-Host 'debug 日志：未打开（info 级）' -ForegroundColor DarkGray
            }
            if ($env:HEINRICH_TSF_LOG) {
                Write-Host "环境变量 HEINRICH_TSF_LOG=$($env:HEINRICH_TSF_LOG) 优先于标志文件。" -ForegroundColor Yellow
            }
        }
    }
}

if ($Action -eq 'debug') {
    Set-DebugSwitch -Value $Switch
    exit 0
}

if ($Action -eq 'log') {
    Show-Log
    exit 0
}

if (-not (Test-Path $setup)) {
    Write-Host "找不到 $setup" -ForegroundColor Red
    Write-Host '先跑 tools\build.ps1 生成产物。'
    exit 1
}

# status 不需要管理员，install / uninstall 需要。
if ($Action -ne 'status' -and -not (Test-Elevated)) {
    Write-Host "「$Action」需要管理员权限，正在以管理员身份重新运行本脚本…" -ForegroundColor Yellow
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, $Action, '-DistDir', $DistDir)
    try {
        Start-Process -FilePath 'powershell.exe' -ArgumentList $argList -Verb RunAs | Out-Null
    }
    catch {
        Write-Host '未能获得管理员权限（UAC 被取消？）' -ForegroundColor Red
        exit 1
    }
    Write-Host '已在新窗口中以管理员身份启动，请看那个窗口的输出。'
    exit 0
}

$ErrorActionPreference = 'Continue'
& $setup $Action 2>&1 | ForEach-Object { Write-Host $_ }
$code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'

# install 之后顺手把日志位置提示出来，省得再去猜。
if ($Action -eq 'install') {
    Write-Host ''
    Show-Log
}

exit $code
