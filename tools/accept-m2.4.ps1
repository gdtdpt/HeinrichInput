#requires -Version 5.1
<#
.SYNOPSIS
    M2.4 学习落库验收：读学习库、跑结构断言、与基线对比。

.DESCRIPTION
    **只读打开**（`mode=ro`），不会改动你的库。

    三种用法：

      -Snapshot   打几个字**之前**跑一次，记下基线
      -Check      打完字**之后**跑，与本轮基线对比，并跑一遍结构断言
      不给参数     只打印现状 + 结构断言

    「有记录到达」这件事光看库不够——还要能确认 DLL 真的把记录发过来了。
    所以这里比的是**差值**：基线到 -Check 之间哪些词的 n 涨了。

.PARAMETER Word
    -Check 时额外断言这个词的 n 至少涨了 -Expect。用于「我按了 3 次，就该涨 3」。

.PARAMETER Expect
    与 -Word 配合的期望增量，默认 1。
    参数名不叫 -N：PowerShell 变量名不区分大小写，本脚本里 `$n` 是表格循环变量，
    `$N` 会被它覆盖（第一次自测就踩了，期望 1 变成期望 4）。

.PARAMETER DbPath
    学习库路径，默认 `%LOCALAPPDATA%\Heinrich\learn.db`。
    注意不能叫 `-Db`：PowerShell 把 `db` 当成 `-Debug` 的别名，会报参数冲突。

.PARAMETER Top
    打印前多少条候选（按上屏次数降序），默认 15。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -Snapshot
    # 去记事本里打几个词，比如 vbg 按空格、vbg 按 2、hao 按 1
    powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -Check

.EXAMPLE
    # 断言「好」在这一轮里正好涨了 3
    powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -Check -Word 好 -Expect 3

.EXAMPLE
    # 换一个库看（配合 heinrich-server --db 做隔离实验）
    powershell -ExecutionPolicy Bypass -File tools\accept-m2.4.ps1 -DbPath .\tmp\learn.db
#>
[CmdletBinding()]
param(
    [switch]$Snapshot,
    [switch]$Check,
    [string]$Word,
    [int]$Expect = 1,
    [string]$DbPath,
    [int]$Top = 15
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$env:PYTHONIOENCODING = 'utf-8'

# ---------------------------------------------------------------- 前置

if ($Snapshot -and $Check) {
    throw '一次只做一件：-Snapshot 或 -Check'
}

if (-not $DbPath) {
    $DbPath = Join-Path $env:LOCALAPPDATA 'Heinrich\learn.db'
}
$DbPath = [System.IO.Path]::GetFullPath($DbPath)

# 脚本不再要求用户装什么：机器上只要有 python 3（自带 sqlite3）就够。
# 优先 `python`，其次 `py -3`（Windows 上的启动器写法）。
$python = $null
$pyArgs = @()
foreach ($cand in @(
        @{ Exe = 'python'; Args = @() },
        @{ Exe = 'py'; Args = @('-3') }
    )) {
    $cmd = Get-Command $cand.Exe -ErrorAction SilentlyContinue
    if (-not $cmd) { continue }
    try {
        $probe = & $cmd.Source @($cand.Args + @('-c', 'import sqlite3')) 2>$null
        if ($LASTEXITCODE -eq 0) { $python = $cmd.Source; $pyArgs = $cand.Args; break }
    } catch { }
}
if (-not $python) {
    throw '找不到可用的 python 3（需要它自带的 sqlite3 模块来读库）。装一个或改用 DB Browser for SQLite。'
}

$baselinePath = Join-Path $env:TEMP 'heinrich-accept-m2.4.json'

# ---------------------------------------------------------------- 读库

# 交给 python 的活儿只有一件：把状态读成 JSON。断言与排版都留在 PowerShell 里，
# 这样报错信息是中文、可点开，而不是一串 python traceback。
$readerPy = @'
import json, os, sqlite3, sys, time
from pathlib import Path

db, top = sys.argv[1], int(sys.argv[2])
out = {"db": db, "exists": os.path.exists(db)}

def fsize(p):
    try:
        return os.path.getsize(p)
    except OSError:
        return 0

out["bytes"] = fsize(db)
out["wal_bytes"] = fsize(db + "-wal")

if not out["exists"]:
    print(json.dumps(out, ensure_ascii=False)); raise SystemExit(0)

con = sqlite3.connect(Path(db).as_uri() + "?mode=ro", uri=True, timeout=3)
try:
    tables = [r[0] for r in con.execute(
        "select name from sqlite_master where type='table' order by name")]
    out["tables"] = tables

    def scalar(sql, default=None):
        try:
            row = con.execute(sql).fetchone()
            return row[0] if row and row[0] is not None else default
        except sqlite3.Error:
            return default

    out["schema"] = scalar("select v from meta where k='schema'")
    out["now"] = int(time.time())
    out["rows"] = scalar("select count(*) from usage", 0)
    out["total_n"] = scalar("select coalesce(sum(n),0) from usage", 0)
    out["future"] = scalar(
        "select count(*) from usage where last_used_at > %d" % (int(time.time()) + 300), 0)
    out["zero_n"] = scalar("select count(*) from usage where n < 1", 0)
    out["bad_tier"] = scalar(
        "select count(*) from usage where tier not in (0,1,2)", 0)
    out["empty_key"] = scalar(
        "select count(*) from usage where trim(key) = ''", 0)
    out["user_words"] = scalar("select count(*) from user_words", 0)

    rows = con.execute(
        "select key, n, last_used_at, tier from usage "
        "order by n desc, last_used_at desc limit ?", (top,)).fetchall()
    out["top"] = [[k, n, t, tier] for k, n, t, tier in rows]
    # 全量词 → n，用于差值比对
    out["all"] = {k: [n, t] for k, n, t in con.execute(
        "select key, n, last_used_at from usage")}
finally:
    con.close()

print(json.dumps(out, ensure_ascii=False))
'@

$tmpPy = Join-Path $env:TEMP ('heinrich-accept-{0}.py' -f $PID)
$utf8 = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($tmpPy, $readerPy, $utf8)

try {
    $raw = & $python @($pyArgs + @($tmpPy, $DbPath, "$Top"))
    if ($LASTEXITCODE -ne 0) { throw "读取学习库失败（python 退出码 $LASTEXITCODE）" }
} finally {
    Remove-Item $tmpPy -ErrorAction SilentlyContinue
}

$st = ($raw -join "`n") | ConvertFrom-Json

# ---------------------------------------------------------------- 打印

$failed = 0
function Ok([string]$msg) { Write-Host "  ✅ $msg" -ForegroundColor Green }
function Bad([string]$msg) { Write-Host "  ❌ $msg" -ForegroundColor Red; $script:failed++ }
function Info([string]$msg) { Write-Host "  ·  $msg" -ForegroundColor DarkGray }

function Fmt-Size([long]$b) {
    if ($b -ge 1MB) { '{0:N1} MB' -f ($b / 1MB) }
    elseif ($b -ge 1KB) { '{0:N1} KB' -f ($b / 1KB) }
    else { "$b B" }
}

Write-Host ''
Write-Host '== 学习库' -ForegroundColor Cyan
Write-Host "  路径   $($st.db)"
if (-not $st.exists) {
    Write-Host "  文件   不存在（还没打过字，或 server 用了 --memory-db）" -ForegroundColor Yellow
} else {
    $walNote = if ($st.wal_bytes -gt 0) { "，WAL $(Fmt-Size $st.wal_bytes)（未 checkpoint）" } else { '，WAL 已截断' }
    Write-Host "  大小   $(Fmt-Size $st.bytes)$walNote"
    Write-Host "  schema $($st.schema)　表 $($st.tables -join ' / ')"
    Write-Host "  规模   $($st.rows) 个词 / 累计上屏 $($st.total_n) 次"
}

# ---------------------------------------------------------------- 结构断言

Write-Host ''
Write-Host '== 结构断言' -ForegroundColor Cyan

if (-not $st.exists) {
    Bad '学习库不存在，无法断言（先打几个字再来）'
} else {
    if ($st.schema -eq '1') {
        Ok "schema 版本 = 1，与程序一致"
    } else {
        Bad "schema 版本 = $($st.schema)，程序只认 1 —— 比程序新时 server 会降级成内存库（不落盘）"
    }

    foreach ($t in @('usage', 'user_words', 'meta')) {
        if ($st.tables -contains $t) { Ok "表 $t 存在" } else { Bad "表 $t 缺失" }
    }

    if ($st.rows -gt 0) {
        Ok "usage 有 $($st.rows) 条记录（说明记录真的落进去了）"
    } else {
        Bad 'usage 是空的 —— 记录没到达，或 server 用的是内存库'
    }

    if ($st.zero_n -eq 0) { Ok '没有 n < 1 的行（上屏次数至少为 1）' }
    else { Bad "$($st.zero_n) 行的 n < 1，记录逻辑有问题" }

    if ($st.future -eq 0) { Ok '没有未来时间戳（时钟与 MAX(旧,新) 都正常）' }
    else { Bad "$($st.future) 行 last_used_at 在未来，可能是时区或回拨问题" }

    if ($st.bad_tier -eq 0) { Ok 'tier 取值都在 {0,1,2} 内' }
    else { Bad "$($st.bad_tier) 行的 tier 越界" }

    if ($st.empty_key -eq 0) { Ok '没有空词条' }
    else { Bad "$($st.empty_key) 行 key 为空" }

    if ($st.user_words -eq 0) {
        Info 'user_words 为空 —— 正常，自造词要到 M5 才写'
    }
}

# ---------------------------------------------------------------- 现状

if ($st.exists -and $st.rows -gt 0) {
    Write-Host ''
    Write-Host "== 上屏最多的 $Top 个词" -ForegroundColor Cyan
    Write-Host ('  {0,-8} {1,5}  {2,-12} {3}' -f '词', 'n', '最后上屏', 'tier')
    foreach ($r in $st.top) {
        $w, $cnt, $ts, $tierV = $r
        $whenText = [DateTimeOffset]::FromUnixTimeSeconds([long]$ts).ToLocalTime().ToString('MM-dd HH:mm')
        $tierName = @('学习', '固顶', '抑制')[[int]$tierV]
        Write-Host ('  {0,-8} {1,5}  {2,-12} {3}' -f $w, $cnt, $whenText, $tierName)
    }
}

# ---------------------------------------------------------------- 基线对比

if ($Snapshot) {
    $st | ConvertTo-Json -Depth 6 | Set-Content -Path $baselinePath -Encoding UTF8
    Write-Host ''
    Write-Host '== 基线' -ForegroundColor Cyan
    Ok "已记下 $($st.rows) 个词 / 累计 $($st.total_n) 次 → $baselinePath"
    Write-Host ''
    Write-Host '接下来：'
    Write-Host '  1. 打几个词（记事本里就行），记得数清楚每个词按了几次'
    Write-Host '  2. 再跑一次：tools\accept-m2.4.ps1 -Check [-Word 词 -Expect 次数]'
}

if ($Check) {
    Write-Host ''
    Write-Host '== 与基线对比' -ForegroundColor Cyan

    if (-not (Test-Path $baselinePath)) {
        Bad "没有基线（$baselinePath 不存在），先跑一次 -Snapshot"
    } else {
        $base = Get-Content $baselinePath -Raw -Encoding UTF8 | ConvertFrom-Json

        if (-not $st.all -or -not $base.all) {
            Bad '库里读不到 usage 行（基线或现在为空），无法比对'
        } else {
            $grew = @()
            $shrunk = @()
            $added = @()
            foreach ($k in $st.all.PSObject.Properties.Name) {
                $nowN = [int]$st.all.$k[0]
                $nowT = [long]$st.all.$k[1]
                $b = $base.all.PSObject.Properties[$k]
                if (-not $b) { $added += "$k($nowN)"; continue }
                $baseN = [int]$b.Value[0]
                $baseT = [long]$b.Value[1]
                if ($nowN -gt $baseN) { $grew += ('{0} +{1}（{2}→{3}）' -f $k, ($nowN - $baseN), $baseN, $nowN) }
                if ($nowN -lt $baseN) { $shrunk += ('{0} {1}→{2}' -f $k, $baseN, $nowN) }
                if ($nowT -lt $baseT) { Bad "$k 的 last_used_at 往回退了（MAX(旧,新) 没生效）" }
            }

            if ($shrunk.Count -eq 0) { Ok '没有词的 n 变少（单调不减）' }
            else { Bad ("这些词的 n 减少了：" + ($shrunk -join '、')) }

            if ($grew.Count + $added.Count -gt 0) {
                Ok "有记录到达：$($grew.Count) 个词涨了，$($added.Count) 个是新词"
                foreach ($line in $grew) { Info $line }
                if ($added.Count -gt 0) { Info ('新词：' + ($added -join '、')) }
            } else {
                Bad '一条记录都没到 —— 上屏没上报，或记录还在 DLL 队列里（等 200 ms 重跑一次）'
            }

            if ($Word) {
                $nowN = $null
                $baseN = 0
                $p = $st.all.PSObject.Properties[$Word]
                if ($p) { $nowN = [int]$p.Value[0] }
                $bp = $base.all.PSObject.Properties[$Word]
                if ($bp) { $baseN = [int]$bp.Value[0] }

                if ($null -eq $nowN) {
                    Bad "「$Word」不在库里 —— 它没被当成「用户选了一个词」上报"
                } elseif (($nowN - $baseN) -ge $Expect) {
                    Ok "「$Word」涨了 $($nowN - $baseN) 次（期望至少 $Expect）"
                } else {
                    Bad "「$Word」只涨了 $($nowN - $baseN) 次，期望至少 $Expect"
                }
            }

            Write-Host ''
            # 注意是 ToLocalTime()：DateTimeOffset 没有 LocalTime 属性，写成 .LocalTime
            # 会得到 null，接着对 null 调 ToString 报「不能对 Null 值表达式调用方法」。
            $when = [DateTimeOffset]::FromUnixTimeSeconds([long]$base.now).ToLocalTime().ToString('MM-dd HH:mm')
            Write-Host "  基线记于 $when（$($base.rows) 个词 / 累计 $($base.total_n) 次）→ 现在 $($st.rows) 个词 / 累计 $($st.total_n) 次"
        }
    }
}

# ---------------------------------------------------------------- 手工项

Write-Host ''
Write-Host '== 脚本验不了的三件事（要手工）' -ForegroundColor Cyan
Write-Host '  1. 词序真的变了：连打某个次候选 5 次，再打同一个码，看它有没有上移。'
Write-Host '     第一次上屏不计分（§5.1 / D58）：打一次不会改顺序，第二次起才动。'
Write-Host '  2. 三条不报路径：打一个死码（如 abc）、Shift 吐字母、断开 server 后打字，'
Write-Host '     这三种都不该让任何词的 n 上涨。'
Write-Host '  3. 跨重启：tsf.ps1 server stop → 再打同一个码 → 顺序应保持上次学到的样子。'
Write-Host ''

if ($failed -gt 0) {
    Write-Host "$failed 项断言失败。" -ForegroundColor Red
    exit 1
}
Write-Host '断言全部通过。' -ForegroundColor Green
exit 0
