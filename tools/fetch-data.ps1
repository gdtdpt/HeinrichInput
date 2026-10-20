#requires -Version 5.1
<#
.SYNOPSIS
    拉取析字的原始数据到 data/raw/（不进仓库、不进产物）。

.DESCRIPTION
    需要的两份数据：
      1. rime/rime-wubi 的 wubi86.dict.yaml —— 词/码/权重/简码（LGPL-3.0）
      2. 现代汉语常用字表 —— 用于覆盖率校验
    z 码（万能键）条目会在装配时被丢弃，属于预期行为。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\fetch-data.ps1
#>
[CmdletBinding()]
param(
    [string]$RawDir = (Join-Path $PSScriptRoot '..\data\raw')
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
# Invoke-WebRequest 的进度条会把每个 16KB 块的提示都刷到控制台，关掉
$ProgressPreference = 'SilentlyContinue'

$RawDir = [System.IO.Path]::GetFullPath($RawDir)
New-Item -ItemType Directory -Force -Path $RawDir | Out-Null

# 老版本 PowerShell 默认仍是 TLS 1.0，GitHub 会直接拒绝
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$targets = @(
    @{ Name = 'wubi86.dict.yaml'; Url = 'https://raw.githubusercontent.com/rime/rime-wubi/master/wubi86.dict.yaml'; Min = 800KB },
    @{ Name = 'common3500.csv';   Url = ('https://raw.githubusercontent.com/argb/hanzi-data/master/' + [uri]::EscapeDataString('现代汉语常用字表.csv')); Min = 10KB }
)

foreach ($t in $targets) {
    $out = Join-Path $RawDir $t.Name
    Write-Host "下载 $($t.Name) ..." -NoNewline
    try {
        Invoke-WebRequest -Uri $t.Url -OutFile $out -TimeoutSec 120 -UseBasicParsing
    } catch {
        Write-Host " 失败" -ForegroundColor Red
        throw "下载 $($t.Name) 失败：$($_.Exception.Message)"
    }
    $len = (Get-Item $out).Length
    if ($len -lt $t.Min) {
        Write-Host " 异常（仅 $len 字节，可能拿到的是 404 页面）" -ForegroundColor Red
        throw "$($t.Name) 体积过小，检查网络或上游路径是否变化"
    }
    Write-Host (" 完成（{0:N0} 字节）" -f $len) -ForegroundColor Green
}

Write-Host ''
Write-Host "原始数据已就绪：$RawDir" -ForegroundColor Green
Write-Host '下一步：powershell -ExecutionPolicy Bypass -File tools\gen.ps1' -ForegroundColor Yellow
