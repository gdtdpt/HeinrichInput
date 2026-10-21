#requires -Version 5.1
<#
.SYNOPSIS
    拉取亨利输入法的原始数据到 data/raw/（不进仓库、不进产物）。

.DESCRIPTION
    需要的四份数据：
      1. rime/rime-wubi 的 wubi86.dict.yaml —— 词/码/权重/简码（LGPL-3.0）
      2. 现代汉语常用字表 —— 用于覆盖率校验
      3. rime/rime-pinyin-simp 的 pinyin_simp.dict.yaml —— 拼音词/音节/权重（Apache-2.0）
      4. （沿用）常用字表同时用于拼音覆盖率校验
    z 码（万能键）条目会在装配时被丢弃，属于预期行为。

    ⚠️ 拼音词典叫 simp（袖珍简化字），但**含大量繁体与异体字**（鼥、釛、軷…），
    这些条目权重为 0，装配后排在每组末尾，不占首屏。不做额外过滤：它们里有
    人名/地名的用字，砍掉会让某些名字打不出来。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\fetch-data.ps1
#>
[CmdletBinding()]
param(
    [string]$RawDir
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串。
if (-not $RawDir) { $RawDir = Join-Path $PSScriptRoot '..\data\raw' }
# Invoke-WebRequest 的进度条会把每个 16KB 块的提示都刷到控制台，关掉
$ProgressPreference = 'SilentlyContinue'

$RawDir = [System.IO.Path]::GetFullPath($RawDir)
New-Item -ItemType Directory -Force -Path $RawDir | Out-Null

# 老版本 PowerShell 默认仍是 TLS 1.0，GitHub 会直接拒绝
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$targets = @(
    @{ Name = 'wubi86.dict.yaml'; Url = 'https://raw.githubusercontent.com/rime/rime-wubi/master/wubi86.dict.yaml'; Min = 800KB },
    @{ Name = 'common3500.csv';   Url = ('https://raw.githubusercontent.com/argb/hanzi-data/master/' + [uri]::EscapeDataString('现代汉语常用字表.csv')); Min = 10KB },
    @{ Name = 'pinyin_simp.dict.yaml'; Url = 'https://raw.githubusercontent.com/rime/rime-pinyin-simp/master/pinyin_simp.dict.yaml'; Min = 500KB }
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
