#requires -Version 5.1
<#
.SYNOPSIS
    全量校验：格式 → clippy → 单测 → 数据一致性。

.DESCRIPTION
    提交前跑这一个脚本。任何一步失败即停止并返回非零退出码。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\check.ps1
#>
[CmdletBinding()]
param(
    [string]$DataDir = (Join-Path $PSScriptRoot '..\data\wubi86')
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

function Invoke-Step([string]$Name, [scriptblock]$Action) {
    Write-Host "== $Name" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0) {
        Write-Host "失败：$Name（退出码 $LASTEXITCODE）" -ForegroundColor Red
        exit $LASTEXITCODE
    }
}

Invoke-Step 'cargo fmt --check' { & cargo fmt --all -- --check }
Invoke-Step 'cargo clippy' { & cargo clippy --all-targets -- -D warnings }
Invoke-Step 'cargo test' { & cargo test --all }
Invoke-Step '方案数据校验' { & cargo run --release -q -p xizi-cli -- check --data $DataDir }

Write-Host ''
Write-Host '全部通过。' -ForegroundColor Green
