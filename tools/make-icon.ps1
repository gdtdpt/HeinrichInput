# 生成语言列表用的图标（DLL 里没有图标资源，注册表 IconFile 指向这个 ico）。
#
# 为什么要有它：Windows 的输入法列表（Win+空格 / 设置 → 语言）读注册表
# `CTF\TIP\{CLSID}\LanguageProfile\...\IconFile` + `IconIndex`。我们原来把
# IconFile 指到 heinrich_win.dll，但 DLL 里没有图标资源，列表里就是一块空白。
#
# 产物：data\icon\heinrich.ico（16/20/24/32/48 五个尺寸，32bpp）。
# 由 tools\build.ps1 复制到 dist，`heinrich-setup install` 把它当 IconFile。
#
# 用法（改图标后要重跑）：
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1 -Preview C:\tmp\preview.png

param(
    [string]$Out,
    [string]$Preview
)

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type -AssemblyName System.Drawing

if (-not $Out) { $Out = Join-Path $PSScriptRoot '..\data\icon\heinrich.ico' }
$Out = [System.IO.Path]::GetFullPath($Out)
$outDir = Split-Path $Out -Parent
if (-not (Test-Path $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }

# 与语言栏按钮（tsf/src/langbar.rs 的 draw_icon）同一套观感：深灰圆角底 + 白字。
$sizes = @(16, 20, 24, 32, 48)

function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # 圆角矩形（半径 22%，与 GDI 版一致）。
    $r = [Math]::Max(1.0, $size * 0.22)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $w = [double]$size
    $d = $r * 2
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($w - $d, 0, $d, $d, 270, 90)
    $path.AddArc($w - $d, $w - $d, $d, $d, 0, 90)
    $path.AddArc(0, $w - $d, $d, $d, 90, 90)
    $path.CloseFigure()

    $brush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 48, 48, 48))
    $g.FillPath($brush, $path)

    $font = New-Object System.Drawing.Font('Microsoft YaHei', [single]($size * 0.62), [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $sf = New-Object System.Drawing.StringFormat
    $sf.Alignment = [System.Drawing.StringAlignment]::Center
    $sf.LineAlignment = [System.Drawing.StringAlignment]::Center
    $rect = New-Object System.Drawing.RectangleF(0, 0, $size, $size)
    $g.DrawString('亨', $font, [System.Drawing.Brushes]::White, $rect, $sf)

    $font.Dispose(); $brush.Dispose(); $path.Dispose(); $g.Dispose()
    return $bmp
}

# 把 Bitmap 变成 ICO 里的一段 DIB：BITMAPINFOHEADER + 32bpp XOR 位图 + AND 掩码。
function ConvertTo-IconDib([System.Drawing.Bitmap]$bmp) {
    $size = $bmp.Width
    $rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $raw = New-Object byte[] ($stride * $size)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $raw, 0, $raw.Length)
    $bmp.UnlockBits($data)

    $maskRow = [int]([Math]::Floor(($size + 31) / 32) * 4)
    $imgSize = 40 + $size * $size * 4 + $maskRow * $size
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)

    # BITMAPINFOHEADER：高度写两倍（XOR + AND 两张图叠在一起）。
    $bw.Write([int]40)
    $bw.Write([int]$size)
    $bw.Write([int]($size * 2))
    $bw.Write([int16]1)
    $bw.Write([int16]32)
    $bw.Write([int]0)              # BI_RGB
    $bw.Write([int]($size * $size * 4))
    $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0); $bw.Write([int]0)

    # XOR 位图：bottom-up 行序，每像素 BGRA（LockBits 拿到的就是 BGRA）。
    for ($y = $size - 1; $y -ge 0; $y--) {
        $rowStart = $y * $stride
        $bw.Write($raw, $rowStart, $size * 4)
    }
    # AND 掩码：全 0（32bpp 靠 alpha，掩码只为满足格式要求）。
    $zeros = New-Object byte[] ($maskRow * $size)
    $bw.Write($zeros, 0, $zeros.Length)
    $bw.Flush()
    $bytes = $ms.ToArray()
    $bw.Dispose(); $ms.Dispose()
    return $bytes
}

$images = @()
foreach ($s in $sizes) {
    $bmp = New-IconBitmap $s
    $images += , (ConvertTo-IconDib $bmp)
    if ($Preview -and $s -eq 32) {
        $bmp.Save([System.IO.Path]::GetFullPath($Preview), [System.Drawing.Imaging.ImageFormat]::Png)
    }
    $bmp.Dispose()
}

# ICONDIR + ICONDIRENTRY[] + 各图像
$fs = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([int16]0)                 # reserved
$bw.Write([int16]1)                 # type: icon
$bw.Write([int16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $len = $images[$i].Length
    $bw.Write([byte]$s)             # width（256 写 0，我们没有那么大）
    $bw.Write([byte]$s)             # height
    $bw.Write([byte]0)              # 调色板颜色数（32bpp 为 0）
    $bw.Write([byte]0)              # reserved
    $bw.Write([int16]1)             # planes
    $bw.Write([int16]32)            # bitCount
    $bw.Write([int]$len)
    $bw.Write([int]$offset)
    $offset += $len
}
foreach ($img in $images) { $bw.Write($img, 0, $img.Length) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()

Write-Host ("已生成 {0}（{1} 个尺寸：{2}，{3} 字节）" -f $Out, $sizes.Count, ($sizes -join '/'), (Get-Item $Out).Length)
if ($Preview) { Write-Host ("预览图：{0}" -f ([System.IO.Path]::GetFullPath($Preview))) }
