#requires -Version 5.1
<#
.SYNOPSIS
    对外发版：进位版本号 → 全量校验 → 构建安装包 → 打 tag → 打印仍需手工执行的命令。

.DESCRIPTION
    版本规则见 PROJECT.md §10.1：唯一来源是 Cargo.toml 的 [workspace.package] version，
    安装包名 / 应用列表 / heinrich-setup status / 设置窗口标题全从它派生。

    脚本按序做：
      1) 校验在 main 分支、工作区干净
      2) git fetch --tags（拿远端的 tag，避免重名；离线时降级为只用本地 tag）
      3) 按 -Bump 算新版本号并改写 Cargo.toml（只改 [workspace.package] 那一段）
      4) tools\check.ps1（fmt / clippy / 单测 / 数据一致性）
      5) tools\build.ps1 -Installer
      6) 用产物名回验「Cargo 版本 == 安装包名」
      7) 提交版本号改动 + 打 annotated tag

    脚本**不 push、不建 release**：本机 credential.helper 是非交互的选择器，
    `git push` 会报 "terminal prompts disabled"，这类动作必须由人亲手触发。
    末尾会把要执行的命令原样打印出来。

.PARAMETER Bump
    进位哪一位：patch / minor / major。不传则沿用 Cargo.toml 里现有的版本号
    （用于「版本号已手工改好，只差构建和打 tag」的情形）。

.PARAMETER SkipCheck
    跳过 tools\check.ps1。只建议在刚跑过全量校验、只想重出安装包时用。

.PARAMETER NoTag
    只改版本号 + 构建，不提交、不打 tag（预演用）。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\release.ps1 -Bump patch

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File tools\release.ps1 -Bump minor -SkipCheck
#>
[CmdletBinding()]
param(
    [ValidateSet('patch', 'minor', 'major')]
    [string]$Bump,
    [switch]$SkipCheck,
    [switch]$NoTag
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

# 带 [CmdletBinding()] 的脚本用 `powershell -File` 调用时，参数绑定早于脚本作用域建立，
# param 默认值里的 $PSScriptRoot 是空串 —— 所以根目录只能在这里算。
$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Set-Location $root
$mainBranch = 'main'

function Fail([string]$Message) {
    Write-Host "  [x] $Message" -ForegroundColor Red
    exit 1
}

function Step([string]$Message) {
    Write-Host "== $Message" -ForegroundColor Cyan
}

function Invoke-Native([scriptblock]$Action, [string]$What) {
    & $Action
    if ($LASTEXITCODE -ne 0) { Fail "$What 失败（退出码 $LASTEXITCODE）" }
}

# 子脚本用**当前宿主的绝对路径**调起，不写 `powershell`：PATH 里缺
# WindowsPowerShell\v1.0 时按名字查找会直接 CommandNotFound（某些精简 PATH /
# 由别的程序派生的会话就是这样），而 Get-Process 拿到的路径一定可用。
$psExe = (Get-Process -Id $PID).Path
if (-not $psExe) { $psExe = 'powershell.exe' }

# cargo 不一定在当前会话的 PATH 里（用户级 PATH 改过之后要重开终端才继承）。
# 找不到就把默认安装位置补进去，后面的 check.ps1 / build.ps1 作为子进程一起受益。
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $probe = @((Join-Path $env:USERPROFILE '.cargo\bin'))
    if ($env:CARGO_HOME) { $probe += (Join-Path $env:CARGO_HOME 'bin') }
    $found = $probe | Where-Object { Test-Path (Join-Path $_ 'cargo.exe') } | Select-Object -First 1
    if (-not $found) {
        Fail '找不到 cargo（Rust 工具链）——先装 Rust，或把 cargo.exe 所在目录加进 PATH'
    }
    $env:PATH = "$found;$env:PATH"
    Write-Host "  cargo    : 补进 PATH -> $found" -ForegroundColor DarkGray
}

# git 同理：从「别的程序派生的会话」里跑时，PATH 可能只有一小撮目录。
if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    # 逐个拼路径并判空：45 位/32 位混合环境里 ${env:ProgramFiles(x86)} 可能是空串，
    # 直接 Join-Path 会抛「参数 Path 是空值」而不是走进下面的兜底。
    $gitDirs = @()
    if ($env:ProgramFiles) { $gitDirs += (Join-Path $env:ProgramFiles 'Git\cmd') }
    if (${env:ProgramFiles(x86)}) { $gitDirs += (Join-Path ${env:ProgramFiles(x86)} 'Git\cmd') }
    if ($env:LOCALAPPDATA) { $gitDirs += (Join-Path $env:LOCALAPPDATA 'Programs\Git\cmd') }
    $found = $gitDirs | Where-Object { Test-Path (Join-Path $_ 'git.exe') } | Select-Object -First 1
    if (-not $found) {
        Fail '找不到 git ——先装 Git for Windows，或把 git.exe 所在目录加进 PATH'
    }
    $env:PATH = "$found;$env:PATH"
    Write-Host "  git      : 补进 PATH -> $found" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------- 1) 前置检查
Step '前置检查'

$branch = (& git rev-parse --abbrev-ref HEAD).Trim()
if ($branch -ne $mainBranch) {
    Fail "当前在分支 '$branch'，发版必须在 '$mainBranch' 上"
}

$dirty = & git status --porcelain
if ($dirty) {
    Write-Host '  工作区有未提交的改动：' -ForegroundColor Yellow
    $dirty | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
    Fail '先提交（或 stash）再发版，否则 tag 会指在一个「版本号还没进仓库」的提交上'
}
Write-Host "  分支     : $branch（干净）" -ForegroundColor Green

# ---------------------------------------------------------------- 2) 拉 tag
Step '同步远端 tag'

& git fetch --tags --quiet 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host '  拉取失败（离线？）——只用本地 tag 判断重名' -ForegroundColor Yellow
}

# ---------------------------------------------------------------- 3) 算新版本
Step '计算版本号'

$cargoPath = Join-Path $root 'Cargo.toml'
$raw = [System.IO.File]::ReadAllText($cargoPath)
$bytes = [System.IO.File]::ReadAllBytes($cargoPath)
$hasBom = ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)
$nl = if ($raw -like "*`r`n*") { "`r`n" } else { "`n" }
$lines = $raw -split "`r?`n"

$oldVer = $null
$verIdx = -1
$inPkg = $false
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^\s*\[workspace\.package\]') { $inPkg = $true; continue }
    if ($inPkg -and $lines[$i] -match '^\s*\[') { break }
    if ($inPkg -and $lines[$i] -match '^\s*version\s*=\s*"([^"]+)"') {
        $oldVer = $Matches[1]
        $verIdx = $i
        break
    }
}
if (-not $oldVer) { Fail 'Cargo.toml 的 [workspace.package] 里没找到 version = "x.y.z"' }

$parts = $oldVer.Split('.')
if ($parts.Count -ne 3) { Fail "版本号 '$oldVer' 不是 MAJOR.MINOR.PATCH 三段，先手工改成三段" }
$maj = [int]$parts[0]; $min = [int]$parts[1]; $pat = [int]$parts[2]

if ($Bump) {
    switch ($Bump) {
        'major' { $maj++; $min = 0; $pat = 0 }
        'minor' { $min++; $pat = 0 }
        'patch' { $pat++ }
    }
    # 数字段（如 1.08）会被 [int] 吃掉前导零，这里统一按字符串重排更直观
    $newVer = "$maj.$min.$pat"
    Write-Host "  进位     : $oldVer -> $newVer（-$Bump）" -ForegroundColor Green
}
else {
    $newVer = $oldVer
    Write-Host "  沿用     : $newVer（没传 -Bump，用 Cargo.toml 现有的）" -ForegroundColor Green
}

$tag = "v$newVer"
$localTag = & git tag -l $tag
if ($localTag) { Fail "本地已有 tag $tag —— 换一个版本号（-Bump），或先 git tag -d $tag" }
$remoteTag = & git ls-remote --tags origin "refs/tags/$tag" 2>$null
if ($remoteTag) { Fail "远端已有 tag $tag —— 换一个版本号（-Bump）" }

# ---------------------------------------------------------------- 4) 改 Cargo.toml
if ($newVer -ne $oldVer) {
    Step '改写 Cargo.toml'
    $lines[$verIdx] = $lines[$verIdx] -replace '"[^"]+"', "`"$newVer`""
    $enc = New-Object System.Text.UTF8Encoding($hasBom)
    [System.IO.File]::WriteAllText($cargoPath, ($lines -join $nl), $enc)
    Write-Host "  $($lines[$verIdx].Trim())" -ForegroundColor Green
}
else {
    Write-Host '  Cargo.toml 无需改动' -ForegroundColor DarkGray
}

# ---------------------------------------------------------------- 5) 全量校验
if ($SkipCheck) {
    Step '全量校验（已跳过）'
}
else {
    Step '全量校验（fmt / clippy / 单测 / 数据一致性）'
    Invoke-Native { & $psExe -ExecutionPolicy Bypass -File (Join-Path $root 'tools\check.ps1') } '校验'
}

# ---------------------------------------------------------------- 6) 构建 + 安装包
Step '构建 release 产物与安装包'
Invoke-Native { & $psExe -ExecutionPolicy Bypass -File (Join-Path $root 'tools\build.ps1') -Installer } '构建'

$setupPath = Join-Path $root "dist\HeinrichSetup-$newVer.exe"
if (-not (Test-Path $setupPath)) {
    Fail "构建报告成功，但没找到 $setupPath —— 版本号与产物名对不上"
}
$setupSize = [math]::Round((Get-Item $setupPath).Length / 1MB, 1)
Write-Host "  安装包   : $setupPath（$setupSize MB）" -ForegroundColor Green

# ---------------------------------------------------------------- 7) 提交 + 打 tag
if ($NoTag) {
    Step '提交与打 tag（-NoTag，已跳过）'
    Write-Host '  版本号改动留在工作区，产物在 dist\' -ForegroundColor Yellow
}
else {
    Step "提交版本号并打 tag $tag"
    & git add Cargo.toml Cargo.lock
    & git diff --cached --quiet
    if ($LASTEXITCODE -ne 0) {
        & git commit -m "chore(release): v$newVer" --quiet
        if ($LASTEXITCODE -ne 0) { Fail '提交版本号失败' }
        Write-Host "  提交     : chore(release): v$newVer" -ForegroundColor Green
    }
    else {
        Write-Host '  没有需要提交的版本号改动' -ForegroundColor DarkGray
    }
    & git tag -a $tag -m "v$newVer"
    if ($LASTEXITCODE -ne 0) { Fail "打 tag $tag 失败" }
    Write-Host "  tag      : $tag" -ForegroundColor Green
}

# ---------------------------------------------------------------- 8) 手工命令
$repoUrl = (& git remote get-url origin).Trim()
$webUrl = $repoUrl -replace '\.git$', '' -replace '^git@github\.com:', 'https://github.com/'
$relUrl = "$webUrl/releases/new?tag=$tag"
$hasGh = [bool](Get-Command gh -ErrorAction SilentlyContinue)

Write-Host ''
Write-Host '----------------------------------------------------------------' -ForegroundColor Cyan
Write-Host ' 仍需手工执行（push 与本机凭证选择器不兼容，必须由你来触发）' -ForegroundColor Cyan
Write-Host '----------------------------------------------------------------' -ForegroundColor Cyan
Write-Host ''
Write-Host '  # 1. 推送提交与 tag'
Write-Host "  git push origin $mainBranch"
Write-Host "  git push origin $tag"
Write-Host ''
Write-Host '  # 2. 建 release 并附安装包（网页）'
Write-Host "  $relUrl"
Write-Host "  标题：$tag    附件：$setupPath"
Write-Host ''
if ($hasGh) {
    Write-Host '  # 2-替代. 用 gh 一条命令完成（本机已装 gh）'
    Write-Host "  gh release create $tag `"$setupPath`" --title `"$tag`" --generate-notes"
}
else {
    Write-Host '  # 2-替代. 装了 GitHub CLI 之后可以一条命令完成：'
    Write-Host "  winget install --id GitHub.cli"
    Write-Host "  gh release create $tag `"$setupPath`" --title `"$tag`" --generate-notes"
}
Write-Host ''
Write-Host "  发布后记得到 PROJECT.md §12 补一行 $newVer 的变更记录（若还没写）。" -ForegroundColor DarkGray
