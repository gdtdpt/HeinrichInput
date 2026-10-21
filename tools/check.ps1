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
    [string]$DataDir,
    [string]$PinyinDir
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串。
if (-not $DataDir) { $DataDir = Join-Path $PSScriptRoot '..\data\wubi86' }
if (-not $PinyinDir) { $PinyinDir = Join-Path $PSScriptRoot '..\data\pinyin' }

function Invoke-Step([string]$Name, [scriptblock]$Action) {
    Write-Host "== $Name" -ForegroundColor Cyan
    # Windows PowerShell 5.1 会把原生程序写到 stderr 的**每一行**包成
    # NativeCommandError（cargo 的进度、编译警告、clippy 提示全走 stderr），
    # 配合 $ErrorActionPreference='Stop' 会直接终止脚本——于是「检查其实通过了
    # 但脚本崩了」。这里临时放开 Stop，成败只看 $LASTEXITCODE。
    $ErrorActionPreference = 'Continue'
    & $Action 2>&1 | ForEach-Object { Write-Host $_ }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) {
        Write-Host "失败：$Name（退出码 $code）" -ForegroundColor Red
        exit $code
    }
}

Invoke-Step 'cargo fmt --check' { & cargo fmt --all -- --check }
Invoke-Step 'cargo clippy' { & cargo clippy --all-targets -- -D warnings }
Invoke-Step 'cargo test' { & cargo test --all }
Invoke-Step '方案数据校验' { & cargo run --release -q -p heinrich-cli -- check --data $DataDir }
# 拼音词库与五笔词库是两份独立的产物，各有各的验收项（拼音那条要求常用字 100%）。
# 缺了这一步，「拼音数据装错了」要等到真机打拼音时才发现。
Invoke-Step '拼音数据校验' { & cargo run --release -q -p heinrich-cli -- check-pinyin --data $PinyinDir }
# server 自测要真装载 13.6 万条词库并跑几条查询：能过说明「装得进、查得对」，
# 这一步不需要真的开管道客户端，所以在 CI/提交前就能跑。
Invoke-Step 'server 自测' { & cargo run --release -q -p heinrich-server -- --selftest --scheme-dir $DataDir }
# 候选窗是唯一没法单测的部分（HWND/D2D/DWrite 全靠肉眼），但它的**行为**可以自测：
# 会不会抢前台、点候选命中的是不是同一个、点箭头会不会误选词、窗口尺寸与布局是否一致。
# 需要交互桌面（要真建一个窗口），所以这一条只在有桌面会话的机器上有意义。
Invoke-Step '候选窗自测' { & cargo run --release -q -p heinrich-cand-preview -- --selftest }

Write-Host ''
Write-Host '全部通过。' -ForegroundColor Green
