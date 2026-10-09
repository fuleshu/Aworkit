# Generates the GitHub / Open Graph social preview card for Aworkit.
#   1280x640 PNG, the size GitHub and most link unfurlers expect.
# Run:  powershell -NoProfile -ExecutionPolicy Bypass -File screenshots\generate-social-preview.ps1
# Output: screenshots\social-preview.png        (recommended - workflow editor)
#         screenshots\social-preview-chat.png   (alternative - chat + Run details)
# Edit the two Render-Card calls at the bottom to change the wording, or the
# -cropX/-cropY/-cropSide arguments to frame a different part of a screenshot.

Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = "Stop"
$outDir = Join-Path $PSScriptRoot ""
$dot = [char]0x00B7          # middle dot as a code point, so this file stays pure ASCII

function Get-FontFamily([string[]]$candidates) {
  $installed = New-Object System.Drawing.Text.InstalledFontCollection
  $names = @{}
  foreach ($f in $installed.Families) { $names[$f.Name] = $true }
  foreach ($c in $candidates) { if ($names.ContainsKey($c)) { return $c } }
  return "Arial"
}

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90)
  $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
  $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  return $p
}

function Add-RadialGlow($g, [float]$cx, [float]$cy, [float]$radius, $color) {
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $path.AddEllipse($cx - $radius, $cy - $radius, $radius * 2, $radius * 2)
  $brush = New-Object System.Drawing.Drawing2D.PathGradientBrush($path)
  $brush.CenterColor = $color
  $brush.SurroundColors = @([System.Drawing.Color]::FromArgb(0, $color.R, $color.G, $color.B))
  $brush.FocusScales = New-Object System.Drawing.PointF(0.02, 0.02)
  $g.FillPath($brush, $path)
  $brush.Dispose(); $path.Dispose()
}

$uiFamily   = Get-FontFamily @("Segoe UI Variable Display", "Segoe UI", "Tahoma")
$bodyFamily = Get-FontFamily @("Segoe UI Variable Text", "Segoe UI", "Tahoma")
Write-Host "Fonts -> UI: $uiFamily | Body: $bodyFamily"

$W = 1280; $H = 640

function Render-Card([string]$shotPath, [string]$outPath, [string]$tagline1, [string]$tagline2,
                     [int]$cropX, [int]$cropY, [int]$cropSide) {
  $bmp = New-Object System.Drawing.Bitmap($W, $H, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
  $g.Clear([System.Drawing.Color]::FromArgb(255, 11, 13, 17))

  Add-RadialGlow $g 130 40 620 ([System.Drawing.Color]::FromArgb(64, 91, 140, 255))
  Add-RadialGlow $g 1180 640 620 ([System.Drawing.Color]::FromArgb(52, 124, 92, 255))

  $gridPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(9, 255, 255, 255), 1)
  for ($x = 0; $x -le $W; $x += 40) { $g.DrawLine($gridPen, $x, 0, $x, $H) }
  for ($y = 0; $y -le $H; $y += 40) { $g.DrawLine($gridPen, 0, $y, $W, $y) }
  $gridPen.Dispose()

  # ---------- screenshot ----------
  $shotSize = 516
  $sx = $W - 46 - $shotSize
  $sy = [int](($H - $shotSize) / 2)

  $glowBehind = New-Object System.Drawing.Drawing2D.GraphicsPath
  $glowBehind.AddEllipse(($sx - 30), ($sy + 40), ($shotSize + 60), ($shotSize - 60))
  $behind = New-Object System.Drawing.Drawing2D.PathGradientBrush($glowBehind)
  $behind.CenterColor = [System.Drawing.Color]::FromArgb(70, 91, 140, 255)
  $behind.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 91, 140, 255))
  $behind.FocusScales = New-Object System.Drawing.PointF(0.05, 0.05)
  $g.FillPath($behind, $glowBehind)
  $behind.Dispose(); $glowBehind.Dispose()

  $src = [System.Drawing.Image]::FromFile($shotPath)
  if ($cropSide -gt 0) {
    $side = $cropSide; $srcX = $cropX; $srcY = $cropY
    if ($srcX + $side -gt $src.Width)  { $srcX = $src.Width - $side }
    if ($srcY + $side -gt $src.Height) { $srcY = $src.Height - $side }
    if ($srcX -lt 0) { $srcX = 0 }
    if ($srcY -lt 0) { $srcY = 0 }
  } else {
    $side = [Math]::Min($src.Width, $src.Height)
    $srcX = [int](($src.Width - $side) / 2)
    $srcY = [int](($src.Height - $side) * 0.30)
    if ($srcY + $side -gt $src.Height) { $srcY = $src.Height - $side }
  }

  $clip = New-RoundedPath $sx $sy $shotSize $shotSize 14
  $state = $g.Save()
  $g.SetClip($clip)
  $destRect = New-Object System.Drawing.Rectangle($sx, $sy, $shotSize, $shotSize)
  $g.DrawImage($src, $destRect, $srcX, $srcY, $side, $side, [System.Drawing.GraphicsUnit]::Pixel)
  $g.Restore($state)
  $src.Dispose()

  $borderPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(38, 255, 255, 255), 1.4)
  $g.DrawPath($borderPen, $clip)
  $borderPen.Dispose(); $clip.Dispose()

  # ---------- text ----------
  $left = 64

  $pillFont = New-Object System.Drawing.Font($bodyFamily, 13, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $pillText = "APACHE-2.0   $dot   OPEN SOURCE   $dot   v0.1.0"
  $pillSize = $g.MeasureString($pillText, $pillFont)
  $pillW = $pillSize.Width + 16; $pillH = 34; $pillY = 148
  $pillPath = New-RoundedPath $left $pillY $pillW $pillH 17
  $pillFill = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(26, 91, 140, 255))
  $g.FillPath($pillFill, $pillPath)
  $pillPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(82, 122, 158, 255), 1)
  $g.DrawPath($pillPen, $pillPath)
  $pillTextBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 159, 180, 230))
  $g.DrawString($pillText, $pillFont, $pillTextBrush, ($left + 8), ($pillY + 8))
  $pillFill.Dispose(); $pillPen.Dispose(); $pillTextBrush.Dispose(); $pillPath.Dispose()

  $titleFont = New-Object System.Drawing.Font($uiFamily, 104, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $titleX = [float]($left - 5)
  $titleRect = New-Object System.Drawing.RectangleF($titleX, [float]196, [float]600, [float]132)
  $titleGrad = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
      $titleRect, [System.Drawing.Color]::FromArgb(255, 255, 255, 255),
      [System.Drawing.Color]::FromArgb(255, 150, 183, 255), 12.0)
  $g.DrawString("Aworkit", $titleFont, $titleGrad, $titleRect)

  $subFont = New-Object System.Drawing.Font($uiFamily, 27, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $subBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 111, 155, 255))
  $g.DrawString("Agent Workflow Toolkit", $subFont, $subBrush, $left, 332)
  $subBrush.Dispose()

  $tagFont = New-Object System.Drawing.Font($bodyFamily, 20, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
  $tagBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 166, 176, 190))
  $g.DrawString($tagline1, $tagFont, $tagBrush, $left, 384)
  $g.DrawString($tagline2, $tagFont, $tagBrush, $left, 414)
  $tagBrush.Dispose()

  $divPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(26, 255, 255, 255), 1)
  $g.DrawLine($divPen, $left, 466, ($left + 520), 466)
  $divPen.Dispose()

  $metaFont = New-Object System.Drawing.Font($bodyFamily, 14, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
  $metaBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 122, 133, 150))
  $metaText = "Windows   $dot   Linux   $dot   Local-first   $dot   Any model   $dot   MCP tools"
  $g.DrawString($metaText, $metaFont, $metaBrush, $left, 484)
  $metaBrush.Dispose()

  $hintFont = New-Object System.Drawing.Font($bodyFamily, 15, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
  $hintBrush = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 92, 103, 120))
  $g.DrawString("github.com/fuleshu/Aworkit", $hintFont, $hintBrush, $left, 514)
  $hintBrush.Dispose()

  $g.Dispose()
  $bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Write-Host ("Wrote {0} ({1} bytes)" -f $outPath, (Get-Item $outPath).Length)
}

# Recommended: workflow editor, entire app window (1280x640)
Render-Card (Join-Path $outDir "workflow standard.png") (Join-Path $outDir "social-preview.png") `
  "Design AI-agent workflows on a visual canvas." `
  "Run them locally with any model. Inspect every step." 0 0 0

# Alternative: chat + Run details panel (transparency story)
Render-Card (Join-Path $outDir "chat.png") (Join-Path $outDir "social-preview-chat.png") `
  "Every chat is a visible workflow you can open," `
  "inspect and reshape. Nothing is hidden." 0 0 0
