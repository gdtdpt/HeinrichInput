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
# 字根表是码位键位图（§6.7）的唯一数据源，而且它是**手写**的：130 条里错一条、
# 少一撇，界面上看不出来，只有「一级简码与码表对不上」这一项能被机器抓住。
Invoke-Step '字根表校验' { & cargo run --release -q -p heinrich-cli -- roots check --data $DataDir }
# server 自测要真装载 13.6 万条词库并跑几条查询：能过说明「装得进、查得对」，
# 这一步不需要真的开管道客户端，所以在 CI/提交前就能跑。
# 带上拼音目录：混输是 M3 的主路径，自测会在装了拼音时额外断言「hao 判拼音、
# vbg 仍判五笔」；没有拼音数据时它自动退化成纯五笔的断言，同样会过。
Invoke-Step 'server 自测' { & cargo run --release -q -p heinrich-server -- --selftest --scheme-dir $DataDir --pinyin-dir $PinyinDir }
# 候选窗是唯一没法单测的部分（HWND/D2D/DWrite 全靠肉眼），但它的**行为**可以自测：
# 会不会抢前台、点候选命中的是不是同一个、点箭头会不会误选词、窗口尺寸与布局是否一致。
# 需要交互桌面（要真建一个窗口），所以这一条只在有桌面会话的机器上有意义。
# 带 `--hint --split 2`：跑的是「两段 + 码提示 chip」那条最复杂的排版路径，
# 顺带断言 chip 不越出自己的项、也不压到邻居。
Invoke-Step '候选窗自测' { & cargo run --release -q -p heinrich-cand-preview -- --selftest --hint --split 2 }
# 键位图（§6.7）是第二个分层窗口：尺寸、越界、抢前台这三类问题与候选窗同源，
# 但数据换成真实字根表，所以它同时验「roots.toml 装得进几何层、13 个字的键帽
# 也放得下」。`vbg` 覆盖的是最常走的 3 码路径。
Invoke-Step '键位图自测' { & cargo run --release -q -p heinrich-cand-preview -- --keycaps vbg --selftest }

# 通配与多段（M3.5）是纯逻辑，CLI 的 `look` 一次查询就能验，不需要真机：
# `vbz` 要列出 `vb` 开头的字且**每条带自己的全码**（不带码等于看了一屏无关的字）；
# `hao'vb` 要只出词典真词、且 chip 嵌在词中（好[vbg]好，D64：挂词尾会被误读成
# 整个词的五笔码）。
Invoke-Step '通配 / 多段自测' {
    $w = & cargo run --release -q -p heinrich-cli -- look vbz --data $DataDir
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if (-not ($w -match '好\s+vbg')) {
        Write-Host "通配没给出「好 -> vbg」：`n$w" -ForegroundColor Red
        exit 1
    }
    if (-not ($w -match '每条都带码')) {
        Write-Host "通配没标记 show_codes：`n$w" -ForegroundColor Red
        exit 1
    }
    $m = & cargo run --release -q -p heinrich-cli -- look "hao'vb" --data $DataDir --pinyin $PinyinDir
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if (-not ($m -match '好\[vbg\]好\s+hao''vb')) {
        Write-Host "多段没给出「好[vbg]好 + hao'vb」：`n$m" -ForegroundColor Red
        exit 1
    }
    $n = & cargo run --release -q -p heinrich-cli -- look "hao'ktn" --data $DataDir --pinyin $PinyinDir
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if (-not ($n -match '好\[vbg\]吃')) {
        Write-Host "多段没给出「好[vbg]吃」（真词过滤 + 词中 chip）：`n$n" -ForegroundColor Red
        exit 1
    }
}

# 自定义词组（D65）：加词 → 能查到 → 删词 → 真的消失。
# 这条链要验的是「全量替换」语义：删掉一行后重载，那个词不能再从旧词表里冒出来。
# 全程用临时文件，绝不碰 %APPDATA% 里的真词库——自测不许改用户的词。
Invoke-Step '自定义词自测' {
    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("heinrich-check-words-{0}.tsv" -f $PID)
    Remove-Item $tmp -ErrorAction SilentlyContinue
    try {
        $add = & cargo run --release -q -p heinrich-cli -- word add 微信 --file $tmp --data $DataDir 2>&1
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        # 码是按词组规则推出来的（微信 = 微 tmgt + 信 wyg → tmwy），拼音是逐字兜底
        if (-not ($add -match '已加入「微信」：拼音 weixin · 码 \w{4}')) {
            Write-Host "加词没给出「微信 / weixin / 4 位码」：`n$add" -ForegroundColor Red
            exit 1
        }
        $list = & cargo run --release -q -p heinrich-cli -- word list --file $tmp --data $DataDir
        if (-not ($list -match '微信')) {
            Write-Host "词表里没有「微信」：`n$list" -ForegroundColor Red
            exit 1
        }
        $hit = & cargo run --release -q -p heinrich-cli -- look tmwy --data $DataDir --pinyin $PinyinDir --words $tmp
        if (-not ($hit -match '微信\s+tmwy')) {
            Write-Host "加了词却查不到「微信」：`n$hit" -ForegroundColor Red
            exit 1
        }
        & cargo run --release -q -p heinrich-cli -- word rm 微信 --file $tmp --data $DataDir | Out-Null
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $gone = & cargo run --release -q -p heinrich-cli -- look tmwy --data $DataDir --pinyin $PinyinDir --words $tmp
        if ($gone -match '微信') {
            Write-Host "删了词却还在（全量替换没生效）：`n$gone" -ForegroundColor Red
            exit 1
        }
    }
    finally {
        Remove-Item $tmp -ErrorAction SilentlyContinue
    }
}

# 设置窗口（M4a）：窗口本身要人看，但**数据层**可以自测——它在临时目录里跑一遍
# 「读空文件 / 加词 / 原子落盘（不留 .tmp）/ 重读 / 推导（拼音反查 + 码按规则推）/
# 删除与恢复 / 坏行只警告」，正是词组页真正会出事的那几处。
# 同样绝不碰 %APPDATA%：自测改用户词库是不可接受的副作用。
Invoke-Step '设置窗口自测' { & cargo run --release -q -p heinrich-settings -- --selftest --data $DataDir }

# 窗口层的自测：只查 z 序与命中测试。截图看着完美、鼠标点哪儿都没反应这类问题
# （F44：Tab 盖住全部按钮）只有查命中才查得出来，肉眼和截图都漏。
Invoke-Step '设置窗口 z 序/命中自测' { & cargo run --release -q -p heinrich-settings -- --selftest-ui --data $DataDir }

Write-Host ''
Write-Host '全部通过。' -ForegroundColor Green
