param([string]$OutDir = $PSScriptRoot)

Add-Type -AssemblyName System.Drawing

$families = [System.Drawing.FontFamily]::Families | ForEach-Object { $_.Name }
$fontName = if ($families -contains "Segoe UI Black") { "Segoe UI Black" }
            elseif ($families -contains "Arial Black") { "Arial Black" }
            else { "Segoe UI" }

function New-RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-SheetIcon([int]$size) {
    $s = $size / 256.0
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $sheet = New-RoundedPath (44 * $s) (20 * $s) (168 * $s) (216 * $s) (14 * $s)
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 255, 255))
    $border = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 200, 210, 217)), (2.6 * $s)
    $g.FillPath($white, $sheet)
    $g.DrawPath($border, $sheet)

    $fold = New-Object System.Drawing.Drawing2D.GraphicsPath
    $fold.AddPolygon(@(
            (New-Object System.Drawing.PointF ((170 * $s), (20 * $s))),
            (New-Object System.Drawing.PointF ((212 * $s), (20 * $s))),
            (New-Object System.Drawing.PointF ((212 * $s), (62 * $s)))
        ))
    $foldBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 229, 236, 240))
    $g.FillPath($foldBrush, $fold)
    $g.DrawPath($border, $fold)

    $font = New-Object System.Drawing.Font $fontName, (152 * $s), ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $teal = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 35, 157, 173))
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = [System.Drawing.StringAlignment]::Center
    $fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
    $rect = New-Object System.Drawing.RectangleF ((44 * $s), (64 * $s), (168 * $s), (172 * $s))
    $g.DrawString("t", $font, $teal, $rect, $fmt)

    $font.Dispose(); $fmt.Dispose(); $teal.Dispose(); $foldBrush.Dispose(); $fold.Dispose()
    $border.Dispose(); $white.Dispose(); $sheet.Dispose(); $g.Dispose()
    return $bmp
}

function New-TileIcon([int]$size) {
    $s = $size / 256.0
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $tile = New-RoundedPath (10 * $s) (10 * $s) (236 * $s) (236 * $s) (52 * $s)
    $c1 = [System.Drawing.Color]::FromArgb(255, 62, 192, 208)
    $c2 = [System.Drawing.Color]::FromArgb(255, 27, 132, 148)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush (
        (New-Object System.Drawing.PointF 0, (10 * $s)),
        (New-Object System.Drawing.PointF 0, (246 * $s)), $c1, $c2)
    $g.FillPath($grad, $tile)

    $font = New-Object System.Drawing.Font $fontName, (176 * $s), ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $whiteBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 255, 255))
    $fmt = New-Object System.Drawing.StringFormat
    $fmt.Alignment = [System.Drawing.StringAlignment]::Center
    $fmt.LineAlignment = [System.Drawing.StringAlignment]::Center
    $rect = New-Object System.Drawing.RectangleF ((10 * $s), (28 * $s), (236 * $s), (200 * $s))
    $g.DrawString("t", $font, $whiteBrush, $rect, $fmt)

    $font.Dispose(); $fmt.Dispose(); $whiteBrush.Dispose(); $grad.Dispose()
    $tile.Dispose(); $g.Dispose()
    return $bmp
}

function Save-Ico($bitmaps, [string]$path) {
    $payloads = @()
    foreach ($b in $bitmaps) {
        $ms = New-Object System.IO.MemoryStream
        $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $payloads += , @($b.Width, $ms.ToArray())
        $ms.Dispose()
    }
    $fs = [System.IO.File]::Create($path)
    $bw = New-Object System.IO.BinaryWriter($fs)
    $bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$payloads.Count)
    $offset = 6 + 16 * $payloads.Count
    foreach ($entry in $payloads) {
        $dim = if ($entry[0] -ge 256) { 0 } else { $entry[0] }
        $bw.Write([Byte]$dim); $bw.Write([Byte]$dim); $bw.Write([Byte]0); $bw.Write([Byte]0)
        $bw.Write([UInt16]1); $bw.Write([UInt16]32)
        $bw.Write([UInt32]$entry[1].Length); $bw.Write([UInt32]$offset)
        $offset += $entry[1].Length
    }
    foreach ($entry in $payloads) { $bw.Write($entry[1]) }
    $bw.Flush(); $bw.Dispose(); $fs.Dispose()
}

function New-Preview($factory, [string]$path, [string]$title) {
    $sizes = @(256, 48, 32, 16)
    $w = 760; $h = 420
    $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $light = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 246, 247, 249))
    $dark = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 30, 32, 36))
    $g.FillRectangle($light, 0, 0, $w, 210); $g.FillRectangle($dark, 0, 210, $w, 210)
    $font = New-Object System.Drawing.Font "Segoe UI", 12
    $fg = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 90, 96, 104))
    $g.DrawString("$title  (256 / 48 / 32 / 16)", $font, $fg, 16, 6)
    $x = 20
    foreach ($size in $sizes) {
        $icon = & $factory $size
        $y = 40 + (160 - $size) / 2
        $g.DrawImage($icon, $x, $y, $size, $size)
        $g.DrawImage($icon, $x, 250 + (160 - $size) / 2, $size, $size)
        $x += $size + 28
        $icon.Dispose()
    }
    $zoom16 = & $factory 16; $zoom32 = & $factory 32
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
    $g.DrawImage($zoom16, 20, 210 + 30, 160, 160)
    $g.DrawImage($zoom32, 220, 210 + 30, 160, 160)
    $g.DrawString("16px x10", $font, $fg, 20, 380)
    $g.DrawString("32px x5", $font, $fg, 220, 380)
    $zoom16.Dispose(); $zoom32.Dispose()
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $font.Dispose(); $fg.Dispose(); $light.Dispose(); $dark.Dispose(); $g.Dispose(); $bmp.Dispose()
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

foreach ($spec in @(@("sheet", { param($n) New-SheetIcon $n }), @("tile", { param($n) New-TileIcon $n }))) {
    $name = $spec[0]; $factory = $spec[1]
    $bitmaps = @(256, 48, 32, 16) | ForEach-Object { & $factory $_ }
    Save-Ico $bitmaps "$OutDir\typst-$name.ico"
    foreach ($b in $bitmaps) { $b.Save("$OutDir\typst-$name-$($b.Width).png", [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose() }
    New-Preview $factory "$OutDir\preview-$name.png" "typst-$name"
}

Get-ChildItem $OutDir | Select-Object -ExpandProperty Name
