param([string]$OutDir)
# Regenerate every icon: powershell -File src-tauri/icons/make-icons.ps1 -OutDir src-tauri/icons
# (also writes preview-16/24.png for checking legibility; delete them afterwards).
# Agora: streams flowing into one conversation. Two ribbons sweep into a modern chat bubble with
# a spark where they meet; one cool gradient (cyan, violet, magenta) on a midnight tile.
Add-Type -AssemblyName System.Drawing
New-Item -ItemType Directory -Force $OutDir | Out-Null

function C($hex, $a = 255) { $c = [System.Drawing.ColorTranslator]::FromHtml($hex); return [System.Drawing.Color]::FromArgb($a, $c.R, $c.G, $c.B) }
function P($x, $y) { return New-Object System.Drawing.PointF $x, $y }

function RoundRect($x, $y, $w, $h, $r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

# A tri-colour gradient along a line.
function Spectrum($x1, $y1, $x2, $y2, $a = 255) {
    $b = New-Object System.Drawing.Drawing2D.LinearGradientBrush (P $x1 $y1), (P $x2 $y2), (C '#000000'), (C '#000000')
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend 3
    $blend.Colors = @((C '#38c8f0' $a), (C '#8b6cf6' $a), (C '#e85aa8' $a))
    $blend.Positions = @(0.0, 0.5, 1.0)
    $b.InterpolationColors = $blend
    return $b
}

# A ribbon: the area between two Bézier curves sharing their ends.
function Ribbon($s, $a, $c1, $c2, $b, $c3, $c4) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $p.AddBezier((P ($a[0]*$s) ($a[1]*$s)), (P ($c1[0]*$s) ($c1[1]*$s)), (P ($c2[0]*$s) ($c2[1]*$s)), (P ($b[0]*$s) ($b[1]*$s)))
    $p.AddBezier((P ($b[0]*$s) ($b[1]*$s)), (P ($c3[0]*$s) ($c3[1]*$s)), (P ($c4[0]*$s) ($c4[1]*$s)), (P ($a[0]*$s) ($a[1]*$s)))
    $p.CloseFigure()
    return $p
}

function New-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 32.0

    $tile = RoundRect (1*$s) (1*$s) (30*$s) (30*$s) (7*$s)
    $night = New-Object System.Drawing.Drawing2D.LinearGradientBrush (P 0 0), (P $size $size), (C '#1e2650'), (C '#0b0f24')
    $g.FillPath($night, $tile)
    $g.SetClip($tile)

    # Ribbons sweeping in from the top left and the bottom right.
    $g.FillPath((Spectrum (0) (4*$s) (18*$s) (12*$s) 235), (Ribbon $s @(-1, 16) @(3, 8.2) @(9, 5.6) @(15, 7.5) @(9.5, 8.6) @(4.5, 11.5) @(-1, 19.5)))
    $g.FillPath((Spectrum (14*$s) (20*$s) (33*$s) (26*$s) 235), (Ribbon $s @(33, 16) @(29, 23.8) @(23, 26.4) @(17, 22) @(22.5, 23.4) @(27.5, 20.5) @(33, 12.5)))

    # The bubble: a gradient shape with its inside cut back to the tile colour.
    $outer = New-Object System.Drawing.Drawing2D.GraphicsPath
    $outer.AddPath((RoundRect (7.2*$s) (7.4*$s) (17.6*$s) (14.6*$s) (6.2*$s)), $false)
    $tail = @((P (9.0*$s) (18.2*$s)), (P (8.0*$s) (25.6*$s)), (P (15.0*$s) (21.4*$s)))
    $outer.FillMode = [System.Drawing.Drawing2D.FillMode]::Winding
    $g.FillPath((Spectrum (7*$s) (7*$s) (25*$s) (25*$s)), $outer)
    $g.FillPolygon((Spectrum (7*$s) (7*$s) (25*$s) (25*$s)), $tail)
    $inner = New-Object System.Drawing.Drawing2D.GraphicsPath
    $inner.AddPath((RoundRect (9.6*$s) (9.8*$s) (12.8*$s) (9.8*$s) (4.4*$s)), $false)
    $inner.FillMode = [System.Drawing.Drawing2D.FillMode]::Winding
    $g.FillPath($night, $inner)

    # Soft light inside, and a four-pointed spark where the streams meet.
    $glow = New-Object System.Drawing.Drawing2D.GraphicsPath
    $glow.AddEllipse((10*$s), (8.6*$s), (12*$s), (12*$s))
    $pgb = New-Object System.Drawing.Drawing2D.PathGradientBrush $glow
    $pgb.CenterColor = C '#b9a6ff' 110
    $pgb.SurroundColors = @((C '#b9a6ff' 0))
    $g.FillPath($pgb, $glow)
    $cx = 16; $cy = 14.6; $r = 3.4; $w = 0.85
    $spark = @((P ($cx*$s) (($cy-$r)*$s)), (P (($cx+$w)*$s) (($cy-$w)*$s)), (P (($cx+$r)*$s) ($cy*$s)), (P (($cx+$w)*$s) (($cy+$w)*$s)),
               (P ($cx*$s) (($cy+$r)*$s)), (P (($cx-$w)*$s) (($cy+$w)*$s)), (P (($cx-$r)*$s) ($cy*$s)), (P (($cx-$w)*$s) (($cy-$w)*$s)))
    $g.FillPolygon((New-Object System.Drawing.SolidBrush (C '#f4f0ff')), $spark)
    $g.ResetClip()
    $g.Dispose()
    return $bmp
}

function PngBytes($bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    return ,$ms.ToArray()
}

foreach ($pair in @(@(32, "32x32.png"), @(128, "128x128.png"), @(512, "icon.png"))) {
    $b = New-Icon $pair[0]
    $b.Save((Join-Path $OutDir $pair[1]), [System.Drawing.Imaging.ImageFormat]::Png)
    $b.Dispose()
}

# ICO with PNG-compressed entries (supported since Windows Vista).
$sizes = @(16, 24, 32, 48, 64, 256)
$images = @()
foreach ($sz in $sizes) { $b = New-Icon $sz; $images += ,(PngBytes $b); $b.Dispose() }
$fs = [System.IO.File]::Create((Join-Path $OutDir "icon.ico"))
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $sz = $sizes[$i]; $len = $images[$i].Length
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32); $w.Write([UInt32]$len); $w.Write([UInt32]$offset)
    $offset += $len
}
foreach ($img in $images) { $w.Write($img) }
$w.Close()
# Small previews to check legibility.
foreach ($sz in @(16, 24)) { $b = New-Icon $sz; $b.Save((Join-Path $OutDir "preview-$sz.png"), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose() }
Get-ChildItem $OutDir | Select-Object Name, Length
# Raw 32x32 RGBA for the tray (embedded by src-tauri/src/tray.rs, so no PNG decoder is needed).
$b = New-Icon 32
$raw = New-Object byte[] (32 * 32 * 4)
for ($y = 0; $y -lt 32; $y++) { for ($x = 0; $x -lt 32; $x++) {
    $c = $b.GetPixel($x, $y); $i = ($y * 32 + $x) * 4
    $raw[$i] = $c.R; $raw[$i + 1] = $c.G; $raw[$i + 2] = $c.B; $raw[$i + 3] = $c.A
} }
[System.IO.File]::WriteAllBytes((Join-Path $OutDir "tray-32.rgba"), $raw)
$b.Dispose()
