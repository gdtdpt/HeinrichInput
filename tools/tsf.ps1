#requires -Version 5.1
<#
.SYNOPSIS
    注册 / 反注册 / 检查「亨利五笔」输入法。

.DESCRIPTION
    heinrich-setup.exe 的 PowerShell 入口。做的事和直接跑 exe 一样，
    额外把三条命令的用法和「装完该看哪里」串起来。

    install / uninstall 需要写 HKLM，会弹 UAC。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 status
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 install

.EXAMPLE
    # 直接看日志尾部（日志按天分文件，自动挑最新的 tsf-<日期>.log）
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log

.EXAMPLE
    # 看每个日志文件多大 / 手动清掉超过 7 天的
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log size
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 log clean

.EXAMPLE
    # 打开逐键 debug 日志（改完要重启宿主程序才生效）
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug on
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 debug off

.EXAMPLE
    # 看常驻 server 的状态 / 手动起停 / 看它的日志
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server stop

.EXAMPLE
    # 验 D18「按键路径零阻塞」：把 server 换成「能握手、查询必超时」的慢实例，
    # 再按提示打一个码，脚本自己读日志判定降级有没有生效。
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server slow
    powershell -ExecutionPolicy Bypass -File tools\tsf.ps1 server slow -SlowMs 1500
#>
[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet('install', 'uninstall', 'status', 'log', 'debug', 'server')]
    [string]$Action = 'status',

    # 第二段子命令：`tsf.ps1 debug on` / `tsf.ps1 server stop`。
    # 位置必须显式声明：PowerShell 按声明顺序分配位置参数，不写 Position 的话
    # `debug on` 里的 `on` 会落到下一个按顺序排列的参数上。
    [Parameter(Position = 1)]
    [ValidateSet('', 'on', 'off', 'status', 'start', 'stop', 'slow', 'size', 'clean')]
    [string]$Sub = '',

    [Parameter(Position = 2)]
    [string]$DistDir,

    [Parameter(Position = 3)]
    [int]$TailLines = 40,

    # `server slow` 的每帧延迟。默认 800ms：**必须小于 DLL 的握手预算 3s**，
    # 否则握手就过不去，测到的是「连不上」而不是「连上了但慢」。
    [Parameter()]
    [int]$SlowMs = 800,

    # `server slow` 自动盯日志的时长。这段时间内去宿主里打字，脚本自己判定。
    [Parameter()]
    [int]$WaitSeconds = 60
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# 默认路径必须在这里算，不能写成 param 的默认值：带 [CmdletBinding()] 的脚本用
# `powershell -File` 调用时，参数绑定早于脚本作用域建立，$PSScriptRoot 还是空串，
# `Join-Path` 会直接抛「无法将参数绑定到参数 Path，因为该参数为空字符串」。
if (-not $DistDir) { $DistDir = Join-Path $PSScriptRoot '..\dist' }

$setup = Join-Path $DistDir 'heinrich-setup.exe'
$serverExe = Join-Path $DistDir 'heinrich-server.exe'
$logDir = Join-Path $env:LOCALAPPDATA 'Heinrich'
$logsDir = Join-Path $logDir 'logs'
$legacyLogPath = Join-Path $logsDir 'tsf.log'
$legacyServerLogPath = Join-Path $logsDir 'server.log'
$debugMarker = Join-Path $logDir 'debug.on'
# 与 crates/heinrich-log 的 KEEP_DAYS 保持一致（那边是权威）。
$keepDays = 7

function Test-Elevated {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-LatestLog {
    # 日志按天分文件（tsf-20261023.log），「当前日志」不是一个固定路径。
    # 按修改时间挑最新的那个，跨天不用改脚本。
    # 必须排掉 `*-test-*`：`cargo test` 的测试宿主写 tsf-test-<日期>.log，
    # 它总是最新的，不排掉的话 `log` 会把单测输出当成真机日志显示。
    param([string]$Pattern)
    $files = @(Get-ChildItem -Path $logsDir -Filter $Pattern -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -notlike '*-test-*' } |
            Sort-Object LastWriteTime -Descending)
    if ($files.Count -gt 0) { return $files[0].FullName }
    return $null
}

function Write-LegacyHint {
    if ((Test-Path $legacyLogPath) -or (Test-Path $legacyServerLogPath)) {
        Write-Host '检测到旧版单文件日志（tsf.log / server.log）：那是升级前的 DLL / server 写的。' -ForegroundColor DarkGray
        Write-Host '新版按天写 tsf-<日期>.log / server-<日期>.log；旧文件可用 `log clean` 删掉。' -ForegroundColor DarkGray
    }
}

function Show-Log {
    $path = Get-LatestLog 'tsf-*.log'
    if (-not $path) {
        Write-Host "还没有日志文件。位置：$logsDir\tsf-<日期>.log" -ForegroundColor Yellow
        Write-Host '先切到「亨利五笔」在记事本里打几个字母，再回来看。'
        Write-LegacyHint
        return
    }
    $info = Get-Item $path
    Write-Host ("== 日志 {0}（{1:N0} 字节，最后写入 {2:HH:mm:ss}）" -f $info.FullName, $info.Length, $info.LastWriteTime) -ForegroundColor Cyan
    # 必须显式 UTF8：Windows PowerShell 5.1 的 Get-Content 默认按 ANSI(936) 解码，
    # 而 DLL 写的是 UTF-8，不指定编码时中文会变成「鈥斺€?鎸夐敭」。
    Get-Content $path -Tail $TailLines -Encoding UTF8
}

function Show-LogFiles {
    if (-not (Test-Path $logsDir)) {
        Write-Host "日志目录还不存在：$logsDir" -ForegroundColor Yellow
        return
    }
    $files = @(Get-ChildItem -Path $logsDir -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending)
    Write-Host "== 日志目录 $logsDir" -ForegroundColor Cyan
    if ($files.Count -eq 0) {
        Write-Host '  （空）' -ForegroundColor DarkGray
        return
    }
    $total = 0
    foreach ($f in $files) {
        $total += $f.Length
        Write-Host ("  {0,-24} {1,10:N0} B  {2:yyyy-MM-dd HH:mm}" -f $f.Name, $f.Length, $f.LastWriteTime)
    }
    Write-Host ("  合计 {0:N0} B；按天保留 {1} 天，单日 8 MiB 后只记 warn/error、32 MiB 停写。" -f $total, $keepDays) -ForegroundColor DarkGray
}

function Clear-OldLogs {
    if (-not (Test-Path $logsDir)) {
        Write-Host "日志目录不存在，无需清理：$logsDir" -ForegroundColor DarkGray
        return
    }
    $cutoff = (Get-Date).AddDays(-$keepDays)
    $victims = @(Get-ChildItem -Path $logsDir -File -ErrorAction SilentlyContinue |
            Where-Object { $_.LastWriteTime -lt $cutoff })
    # 旧格式单文件不带日期，永远不会「自然过期」，这里一并收掉。
    foreach ($legacy in @($legacyLogPath, $legacyServerLogPath)) {
        if (Test-Path $legacy) { $victims += Get-Item $legacy }
    }
    if ($victims.Count -eq 0) {
        Write-Host "没有需要清理的日志（保留 {0} 天内的）。" -f $keepDays -ForegroundColor Green
        return
    }
    foreach ($v in $victims) {
        Write-Host ("  删除 {0}（{1:N0} B，{2:yyyy-MM-dd HH:mm}）" -f $v.Name, $v.Length, $v.LastWriteTime) -ForegroundColor Yellow
        Remove-Item -LiteralPath $v.FullName -Force -ErrorAction SilentlyContinue
    }
    Write-Host '已清理。' -ForegroundColor Green
}

function Set-DebugSwitch {
    param([string]$Value)
    switch ($Value) {
        'on' {
            New-Item -ItemType Directory -Force -Path $logDir | Out-Null
            Set-Content -Path $debugMarker -Value 'debug' -Encoding ASCII
            Write-Host "已打开 debug 日志：$debugMarker" -ForegroundColor Green
            Write-Host '注意：日志级别在每个宿主进程里只解析一次，' -ForegroundColor Yellow
            Write-Host '      必须**重启要观察的程序**（如 Notepad++）才生效。'
        }
        'off' {
            if (Test-Path $debugMarker) { Remove-Item $debugMarker -Force }
            Write-Host '已关闭 debug 日志，回到 info 级。' -ForegroundColor Green
            Write-Host '同样要重启宿主程序才生效。' -ForegroundColor Yellow
        }
        default {
            if (Test-Path $debugMarker) {
                Write-Host "debug 日志：已打开（$debugMarker）" -ForegroundColor Green
            }
            else {
                Write-Host 'debug 日志：未打开（info 级）' -ForegroundColor DarkGray
            }
            if ($env:HEINRICH_TSF_LOG) {
                Write-Host "环境变量 HEINRICH_TSF_LOG=$($env:HEINRICH_TSF_LOG) 优先于标志文件。" -ForegroundColor Yellow
            }
        }
    }
}

function Invoke-ServerAction {
    param([string]$Value)

    $procs = @(Get-Process -Name 'heinrich-server' -ErrorAction SilentlyContinue)

    switch ($Value) {
        'start' {
            if (-not (Test-Path $serverExe)) {
                Write-Host "找不到 $serverExe" -ForegroundColor Red
                Write-Host '先跑 tools\build.ps1。'
                return
            }
            if ($procs.Count -gt 0) {
                Write-Host "已经在跑（pid $($procs[0].Id)），不重复启动。" -ForegroundColor Yellow
                return
            }
            Start-Process -FilePath $serverExe -WindowStyle Hidden | Out-Null
            Start-Sleep -Milliseconds 600
            Write-Host '已启动。注意：正常使用时**不需要手动启动**——DLL 会在切到输入框时自己拉起。' -ForegroundColor Green
        }
        'stop' {
            if ($procs.Count -eq 0) {
                Write-Host '没有在运行的 server。' -ForegroundColor DarkGray
                return
            }
            $procs | ForEach-Object {
                Write-Host "结束 pid $($_.Id)…" -ForegroundColor Yellow
                $_ | Stop-Process -Force -ErrorAction SilentlyContinue
            }
            Write-Host '已结束。下一次按键会自动重连（会重新拉起一个新 server）。' -ForegroundColor Green
        }
        'slow' {
            if (-not (Test-Path $serverExe)) {
                Write-Host "找不到 $serverExe" -ForegroundColor Red
                Write-Host '先跑 tools\build.ps1。'
                return
            }
            if ($SlowMs -ge 3000) {
                Write-Host "SlowMs=$SlowMs ≥ DLL 的握手预算 3s：这样连握手都过不去，" -ForegroundColor Yellow
                Write-Host '测到的是「连不上」而不是「连上了但慢」。想看超时路径请用 800 左右。' -ForegroundColor Yellow
            }

            # 同会话互斥体只有一个赢家：正常实例占着，慢实例会被直接挤掉。
            # 所以先结束它——但 DLL 的后台线程每 200ms 就会拉起一个新的正常实例，
            # 我们只有那 200ms 的窗口。输了就重来（慢实例已被挤掉，重来是安全的）。
            $won = $null
            for ($i = 1; $i -le 6; $i++) {
                Get-Process -Name 'heinrich-server' -ErrorAction SilentlyContinue |
                    Stop-Process -Force -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 40
                $p = Start-Process -FilePath $serverExe `
                    -ArgumentList '--debug-sleep', "$SlowMs", '--idle-exit', '0' `
                    -WindowStyle Hidden -PassThru
                Start-Sleep -Milliseconds 900
                $p.Refresh()
                if (-not $p.HasExited) {
                    $won = $p
                    break
                }
            }
            if (-not $won) {
                Write-Host '没抢到互斥体（一直被自动拉起的正常实例抢先）。再跑一次本命令。' -ForegroundColor Red
                return
            }
            Write-Host "慢 server 已就位 pid $($won.Id)，每帧延迟 $SlowMs ms。" -ForegroundColor Green

            # 日志按天分文件：先记下「当前这个文件已经有多少行」，之后只读新增部分。
            # 跨天时文件名会变，循环里检测到换文件就把 skip 归零。
            $logFile = Get-LatestLog 'tsf-*.log'
            $skip = 0
            if ($logFile) {
                $skip = @(Get-Content $logFile -Encoding UTF8).Count
            }

            Write-Host ''
            Write-Host '现在切到记事本 / Notepad++（不用重启），敲一个【这个宿主进程从没打过】的码再按空格，' -ForegroundColor Cyan
            Write-Host "例如 ab、qq。我在这边自动盯日志，你不用切回来按键；$WaitSeconds 秒内没动静就算超时。" -ForegroundColor Cyan
            Write-Host ''

            # 轮询而不是 Read-Host：让用户只切一次窗口（去打字），判定与收尾都在这里自动完成。
            # try/finally 是为了「按 Ctrl+C 放弃」也不把慢实例留在机器上。
            $deadline = (Get-Date).AddSeconds($WaitSeconds)
            $degraded = $null
            $cached = @()
            $new = @()
            try {
                $lastRemain = -1
                while ((Get-Date) -lt $deadline) {
                    Start-Sleep -Milliseconds 500
                    $latest = Get-LatestLog 'tsf-*.log'
                    if ($latest -and $latest -ne $logFile) {
                        $logFile = $latest
                        $skip = 0
                    }
                    if ($logFile -and (Test-Path $logFile)) {
                        $new = @(Get-Content $logFile -Encoding UTF8 | Select-Object -Skip $skip)
                    }
                    $degraded = $new | Where-Object { $_ -match 'server 不可用' } | Select-Object -First 1
                    if ($degraded) { break }

                    # 命中缓存时提示换码，但**继续等**——不用重新跑脚本。
                    foreach ($line in @($new | Where-Object { $_ -match '提交 .+ → ' })) {
                        if ($cached -notcontains $line) {
                            $cached += $line
                            if ($line -match '提交 (\S+) → (\S+)') {
                                Write-Host ''
                                Write-Host ("  「{0}」走了缓存（早就查过，所以还是出「{1}」）。换一个没打过的码，我继续等。" -f $Matches[1], $Matches[2]) -ForegroundColor Yellow
                            }
                        }
                    }
                    $remain = [int][Math]::Ceiling(($deadline - (Get-Date)).TotalSeconds)
                    if ($remain -ne $lastRemain) {
                        $lastRemain = $remain
                        Write-Host ("`r  等待中，还剩 {0} 秒…        " -f $remain) -NoNewline -ForegroundColor DarkGray
                    }
                }
                Write-Host ''

                Write-Host ''
                Write-Host '== 这一轮新增的日志' -ForegroundColor Cyan
                if ($new.Count -eq 0) {
                    Write-Host '  （没有新增）' -ForegroundColor DarkGray
                }
                else {
                    $new | ForEach-Object { Write-Host "  $_" }
                }

                Write-Host ''
                if ($degraded) {
                    Write-Host '判定：降级路径生效 ✅' -ForegroundColor Green
                    Write-Host ("  server 慢到 {0} ms，宿主一次都没等，编码原样上屏 ⇒ D18 成立。" -f $SlowMs) -ForegroundColor Green
                }
                elseif ($cached.Count -gt 0) {
                    Write-Host '判定：只看到缓存命中，没测到降级 ⚠' -ForegroundColor Yellow
                    Write-Host '  再跑一次本命令，换一个这个宿主进程从没打过的码。' -ForegroundColor Yellow
                }
                else {
                    Write-Host '判定：这一轮日志里什么都没有 ⚠' -ForegroundColor Yellow
                    Write-Host '  三种可能：① 光标不在支持 TSF 的输入框里 ② 宿主里还是旧 DLL（需重启宿主）' -ForegroundColor Yellow
                    Write-Host '  ③ 敲的码一直没按空格（查询只在上屏键上发生）。' -ForegroundColor Yellow
                }
            }
            finally {
                # 收尾自动恢复：撤掉慢实例，DLL 下一次激活会拉起正常的那个。
                # 用户不该为了「撤销一次测试」再记一条命令。
                Get-Process -Name 'heinrich-server' -ErrorAction SilentlyContinue |
                    Stop-Process -Force -ErrorAction SilentlyContinue
                Write-Host ''
                Write-Host '已自动恢复：慢实例结束，正常 server 会在你下次切到输入框时拉起（无需额外命令）。' -ForegroundColor Green
            }
        }
        default {
            if ($procs.Count -eq 0) {
                Write-Host 'server：未运行' -ForegroundColor Yellow
                Write-Host '  这一般是正常的——server 是惰性拉起的，没在用输入法时它可能已经空闲退出。' -ForegroundColor DarkGray
            }
            else {
                foreach ($p in $procs) {
                    Write-Host ("server：运行中 pid {0}，启动于 {1:HH:mm:ss}，内存 {2:N0} KB" -f `
                            $p.Id, $p.StartTime, ($p.WorkingSet64 / 1KB)) -ForegroundColor Green
                }
            }
            # 管道是「连没连上」最直接的证据：名字里带会话 id。
            $pipes = @([System.IO.Directory]::GetFiles('\\.\pipe\') | Where-Object { $_ -match 'heinrich' })
            if ($pipes.Count -gt 0) {
                Write-Host ('管道：' + ($pipes -join ', ')) -ForegroundColor DarkGray
            }
            else {
                Write-Host '管道：无' -ForegroundColor DarkGray
            }
            $serverLog = Get-LatestLog 'server-*.log'
            if ($serverLog) {
                Write-Host ''
                Write-Host ("== server 日志（最后 10 行，{0}）" -f $serverLog) -ForegroundColor Cyan
                Get-Content $serverLog -Tail 10 -Encoding UTF8
            }
            else {
                Write-Host "server 日志还没生成：$logsDir\server-<日期>.log" -ForegroundColor DarkGray
            }
        }
    }
}

if ($Action -eq 'debug') {
    Set-DebugSwitch -Value $Sub
    exit 0
}

if ($Action -eq 'log') {
    switch ($Sub) {
        'size' { Show-LogFiles }
        'clean' { Clear-OldLogs }
        default { Show-Log }
    }
    exit 0
}

if ($Action -eq 'server') {
    Invoke-ServerAction -Value $Sub
    exit 0
}

if (-not (Test-Path $setup)) {
    Write-Host "找不到 $setup" -ForegroundColor Red
    Write-Host '先跑 tools\build.ps1 生成产物。'
    exit 1
}

# status 不需要管理员，install / uninstall 需要。
if ($Action -ne 'status' -and -not (Test-Elevated)) {
    Write-Host "「$Action」需要管理员权限，正在以管理员身份重新运行本脚本…" -ForegroundColor Yellow
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, $Action, '-DistDir', $DistDir)
    try {
        Start-Process -FilePath 'powershell.exe' -ArgumentList $argList -Verb RunAs | Out-Null
    }
    catch {
        Write-Host '未能获得管理员权限（UAC 被取消？）' -ForegroundColor Red
        exit 1
    }
    Write-Host '已在新窗口中以管理员身份启动，请看那个窗口的输出。'
    exit 0
}

$ErrorActionPreference = 'Continue'
& $setup $Action 2>&1 | ForEach-Object { Write-Host $_ }
$code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'

# install 之后顺手把日志位置提示出来，省得再去猜。
if ($Action -eq 'install') {
    Write-Host ''
    Show-Log
}

exit $code
