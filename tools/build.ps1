#requires -Version 5.1
<#
.SYNOPSIS
    构建 release 产物并归集到 dist/。

.DESCRIPTION
    当前（M1.0）产物：
      heinrich-cli.exe    —— 引擎 REPL 与方案校验
      heinrich-gen.exe    —— 数据装配（发版时不需要随包）
      heinrich-setup.exe  —— TSF 注册器
      heinrich_win.dll    —— TSF 前端（必须与 setup 同目录）

    注意 DLL 的文件名是下划线：Cargo 对 cdylib 会把 crate 名里的连字符换成下划线。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build.ps1
#>
[CmdletBinding()]
param(
    [string]$DistDir = (Join-Path $PSScriptRoot '..\dist')
)

# 只对 PowerShell 自己的 cmdlet 用 Stop。原生程序写 stderr 时，Windows PowerShell 5.1
# 会把那些行包成 NativeCommandError，配合 Stop 直接终止脚本——而 cargo 的进度、
# 编译警告全都走 stderr，于是「构建其实成功了但脚本在这一行挂掉」。
# 原生命令的成败一律用 $LASTEXITCODE 判断，所以调用期间临时放开 Stop。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

$ErrorActionPreference = 'Continue'
& cargo build --release --all 2>&1 | ForEach-Object { Write-Host $_ }
$buildExit = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
if ($buildExit -ne 0) {
    Write-Host '构建失败' -ForegroundColor Red
    exit $buildExit
}
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
foreach ($name in @('heinrich-cli.exe', 'heinrich-gen.exe', 'heinrich-setup.exe', 'heinrich_win.dll')) {
    $src = Join-Path $root "target\release\$name"
    if (-not (Test-Path $src)) {
        Write-Host "  缺少 $name" -ForegroundColor Red
        exit 1
    }
    Copy-Item $src -Destination $DistDir -Force
    Write-Host "  已复制 $name"
}

# 方案数据是运行时依赖，与可执行同层放置，目录结构保持一致
$dataOut = Join-Path $DistDir 'data\wubi86'
New-Item -ItemType Directory -Force -Path $dataOut | Out-Null
Copy-Item (Join-Path $root 'data\wubi86\*') -Destination $dataOut -Force

Write-Host ''
Write-Host "产物：$DistDir" -ForegroundColor Green
Get-ChildItem -Recurse $DistDir -File | ForEach-Object {
    Write-Host ("  {0,-24} {1,10:N0} 字节" -f $_.Name, $_.Length)
}
