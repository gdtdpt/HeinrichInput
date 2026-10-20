#requires -Version 5.1
<#
.SYNOPSIS
    构建 release 产物并归集到 dist/。

.DESCRIPTION
    当前（M0）只有两个可执行：
      xizi-cli.exe  —— 引擎 REPL 与方案校验
      xizi-gen.exe  —— 数据装配（发版时不需要随包）
    M1 之后会加入 xizi-server.exe / xizi-win.dll。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\build.ps1
#>
[CmdletBinding()]
param(
    [string]$DistDir = (Join-Path $PSScriptRoot '..\dist')
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

& cargo build --release --all
if ($LASTEXITCODE -ne 0) {
    Write-Host '构建失败' -ForegroundColor Red
    exit $LASTEXITCODE
}

New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
foreach ($name in @('xizi-cli.exe', 'xizi-gen.exe')) {
    $src = Join-Path $root "target\release\$name"
    if (Test-Path $src) {
        Copy-Item $src -Destination $DistDir -Force
        Write-Host "  已复制 $name"
    }
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
