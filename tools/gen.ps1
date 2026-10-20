#requires -Version 5.1
<#
.SYNOPSIS
    装配方案数据：data/raw/ → data/wubi86/，并输出 M0a 校验报告。

.DESCRIPTION
    生成 lexicon.tsv（码/词/权重/简码级别）与 scheme.toml（清单 + 词库哈希），
    报告写到 out/m0a-report.md。产物不含时间戳，同输入必得同字节、同哈希。

    任一验收项不通过时脚本返回非零退出码：
      ① 常用字覆盖 ≥ 99%   ③ 2–3 级简码必须是全码前缀（例外 ≤ 1%）
      ④ 一级简码 25 键、每键至多 1 条

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\gen.ps1
#>
[CmdletBinding()]
param(
    [string]$RawDir,
    [string]$OutDir,
    [string]$Report
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串，
# `Join-Path` 会直接抛「无法将参数绑定到参数 Path，因为该参数为空字符串」。
if (-not $RawDir) { $RawDir = Join-Path $PSScriptRoot '..\data\raw' }
if (-not $OutDir) { $OutDir = Join-Path $PSScriptRoot '..\data\wubi86' }
if (-not $Report) { $Report = Join-Path $PSScriptRoot '..\out\m0a-report.md' }

if (-not (Test-Path (Join-Path $RawDir 'wubi86.dict.yaml'))) {
    throw "缺少 $RawDir\wubi86.dict.yaml，先跑 tools\fetch-data.ps1"
}

& cargo run --release -p heinrich-gen -- --raw $RawDir --out $OutDir --report $Report
if ($LASTEXITCODE -ne 0) {
    Write-Host "装配未通过验收（退出码 $LASTEXITCODE），详见 $Report" -ForegroundColor Yellow
    exit $LASTEXITCODE
}

Write-Host "完成：$Report" -ForegroundColor Green
