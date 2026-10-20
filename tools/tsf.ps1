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
#>
[CmdletBinding()]
param(
    [ValidateSet('install', 'uninstall', 'status', 'log')]
    [string]$Action = 'status',

    [string]$DistDir = (Join-Path $PSScriptRoot '..\dist'),

    [int]$TailLines = 40
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$setup = Join-Path $DistDir 'heinrich-setup.exe'
$logPath = Join-Path $env:LOCALAPPDATA 'Heinrich\logs\tsf.log'

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
