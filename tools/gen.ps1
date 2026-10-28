#requires -Version 5.1
<#
.SYNOPSIS
    装配方案数据：data/raw/ → data/wubi86/ 与 data/pinyin/，并输出校验报告。

.DESCRIPTION
    两套词库，流程与校验项完全不同，用 -Kind 选择：

      wubi   → data/wubi86/lexicon.tsv（码/词/权重/简码级别）+ scheme.toml
               报告 out/m0a-report.md
               验收：① 常用字覆盖 ≥ 99%  ③ 2–3 级简码必须是全码前缀（例外 ≤ 1%）
                     ④ 一级简码 25 键、每键至多 1 条

      pinyin → data/pinyin/lexicon.tsv（无分隔拼音/词/权重）+ syllables.txt + scheme.toml
               报告 out/m3-pinyin-report.md
               验收：① 常用字覆盖必须 100%  ② 拼音串只含 a-z  ③ 音节 ≤ 6 字母

      emoji  → data/wubi86/emoji.toml（词组 → emoji，随方案目录发布）
               报告 out/m4c-emoji-report.md
               验收：① 条目 2000–3000  ② 全为 ≥2 字汉字词组  ③ 一词一条
               依赖 data/pinyin/lexicon.tsv（词来源），要排在 pinyin 之后跑

    产物不含时间戳、不依赖哈希表遍历顺序，同输入必得同字节、同哈希。
    任一验收项不通过时脚本返回非零退出码。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\gen.ps1
    powershell -ExecutionPolicy Bypass -File tools\gen.ps1 -Kind pinyin
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'wubi', 'pinyin', 'emoji')]
    [string]$Kind = 'all',
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

# -Kind all 时两套产物落在不同目录，显式给 -OutDir / -Report 只会让另一套无处可去。
if ($Kind -eq 'all' -and ($OutDir -or $Report)) {
    throw "-Kind all 时不能指定 -OutDir / -Report（两套产物默认落在 data\wubi86 与 data\pinyin）"
}

$jobs = if ($Kind -eq 'all') { @('wubi', 'pinyin', 'emoji') } else { @($Kind) }

foreach ($k in $jobs) {
    if ($k -eq 'wubi') {
        $input = 'wubi86.dict.yaml'
        $defaultOut = '..\data\wubi86'
        $defaultReport = '..\out\m0a-report.md'
    } elseif ($k -eq 'pinyin') {
        $input = 'pinyin_simp.dict.yaml'
        $defaultOut = '..\data\pinyin'
        $defaultReport = '..\out\m3-pinyin-report.md'
    } else {
        $input = 'cldr-annotations-zh.xml'
        $defaultOut = '..\data\wubi86'
        $defaultReport = '..\out\m4c-emoji-report.md'
    }

    if (-not (Test-Path (Join-Path $RawDir $input))) {
        throw "缺少 $RawDir\$input，先跑 tools\fetch-data.ps1"
    }

    $o = if ($OutDir) { $OutDir } else { Join-Path $PSScriptRoot $defaultOut }
    $r = if ($Report) { $Report } else { Join-Path $PSScriptRoot $defaultReport }

    Write-Host "== 装配 $k" -ForegroundColor Cyan
    # Windows PowerShell 5.1 会把原生程序写到 stderr 的每一行包成 NativeCommandError，
    # 配合 $ErrorActionPreference='Stop' 会在 cargo 打印进度时直接中断——看起来像
    # 「装配失败」，其实只是编译器说了句话。成败只看 $LASTEXITCODE。
    $ErrorActionPreference = 'Continue'
    & cargo run --release -p heinrich-gen -- --kind $k --raw $RawDir --out $o --report $r
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) {
        Write-Host "装配 $k 未通过验收（退出码 $code），详见 $r" -ForegroundColor Yellow
        exit $code
    }
    Write-Host "完成：$r" -ForegroundColor Green
}
