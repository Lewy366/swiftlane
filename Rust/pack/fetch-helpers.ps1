# Downloads and SHA-256-verifies Swiftlane's pinned third-party helpers
# (ffmpeg, ffprobe, pdfium.dll, yt-dlp) from Rust/pack/helpers.json.
# Never downloads "latest"; refuses any cached or freshly downloaded file whose SHA-256 differs.
param(
    [Parameter(Mandatory = $true)]
    [string]$OutDir,
    [string]$Manifest = (Join-Path $PSScriptRoot "helpers.json"),
    [string]$CacheDir = ""
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$spec = Get-Content -Path $Manifest -Raw -Encoding UTF8 | ConvertFrom-Json

if (-not $CacheDir) {
    $base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { [IO.Path]::GetTempPath() }
    $CacheDir = Join-Path $base "swiftlane-helpers"
}

New-Item -ItemType Directory -Force -Path $OutDir, (Join-Path $OutDir "licenses"), $CacheDir | Out-Null

function Get-VerifiedFile([string]$Url, [string]$CacheName, [string]$ExpectedSha256, [string]$Label) {
    $expected = $ExpectedSha256.ToLowerInvariant()
    $cached = Join-Path $CacheDir $CacheName
    if (Test-Path $cached) {
        $h = (Get-FileHash $cached -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($h -eq $expected) {
            Write-Host "Using verified cache for $Label ($h)"
            return $cached
        }
        Write-Warning "Cached $Label hash mismatch; removing and re-downloading..."
        Remove-Item $cached -Force
    }
    $tmp = "$cached.part"
    try {
        Write-Host "Downloading $Label..."
        Invoke-WebRequest -Uri $Url -OutFile $tmp -UseBasicParsing
        $actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "$Label SHA-256 mismatch: expected $expected, got $actual"
        }
        Move-Item -Path $tmp -Destination $cached -Force
        Write-Host "Verified $Label ($actual)"
        return $cached
    } finally {
        if (Test-Path $tmp) { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
    }
}

$scratch = Join-Path ([IO.Path]::GetTempPath()) ("swl-helpers-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force -Path $scratch | Out-Null

try {
    # 1. FFmpeg + FFprobe
    $ffZip = Get-VerifiedFile $spec.ffmpeg.url "ffmpeg-$($spec.ffmpeg.version).zip" $spec.ffmpeg.sha256 "FFmpeg $($spec.ffmpeg.version)"
    $ffDir = Join-Path $scratch "ffmpeg"
    Expand-Archive -Path $ffZip -DestinationPath $ffDir -Force
    $ffBin = Get-ChildItem $ffDir -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1
    $fpBin = Get-ChildItem $ffDir -Recurse -Filter "ffprobe.exe" | Select-Object -First 1
    $ffLic = Get-ChildItem $ffDir -Recurse -Filter "LICENSE" | Select-Object -First 1
    if (-not $ffBin -or -not $fpBin) { throw "ffmpeg.exe or ffprobe.exe missing from archive" }
    Copy-Item $ffBin.FullName (Join-Path $OutDir "ffmpeg.exe") -Force
    Copy-Item $fpBin.FullName (Join-Path $OutDir "ffprobe.exe") -Force
    if ($ffLic) { Copy-Item $ffLic.FullName (Join-Path $OutDir "licenses\FFmpeg-LICENSE.txt") -Force }

    # 2. PDFium
    $pdfSlug = $spec.pdfium.version -replace '[^a-zA-Z0-9._-]', '-'
    $pdfTgz = Get-VerifiedFile $spec.pdfium.url "pdfium-$pdfSlug.tgz" $spec.pdfium.sha256 "PDFium $($spec.pdfium.version)"
    $pdfDir = Join-Path $scratch "pdfium"
    New-Item -ItemType Directory -Force -Path $pdfDir | Out-Null
    & tar -xzf $pdfTgz -C $pdfDir
    if ($LASTEXITCODE -ne 0) { throw "tar failed extracting pdfium" }
    $pdfDll = Get-ChildItem $pdfDir -Recurse -Filter "pdfium.dll" | Select-Object -First 1
    $pdfLic = Get-ChildItem $pdfDir -Recurse -Filter "LICENSE" | Select-Object -First 1
    if (-not $pdfDll) { throw "pdfium.dll missing from archive" }
    Copy-Item $pdfDll.FullName (Join-Path $OutDir "pdfium.dll") -Force
    if ($pdfLic) { Copy-Item $pdfLic.FullName (Join-Path $OutDir "licenses\PDFium-LICENSE.txt") -Force }

    # 3. yt-dlp
    $ytExe = Get-VerifiedFile $spec.yt_dlp.url "yt-dlp-$($spec.yt_dlp.version).exe" $spec.yt_dlp.sha256 "yt-dlp $($spec.yt_dlp.version)"
    Copy-Item $ytExe (Join-Path $OutDir "yt-dlp.exe") -Force

    @(
        "Bundled third-party helpers (all downloaded from pinned releases and SHA-256 verified):"
        ""
        "FFmpeg / FFprobe $($spec.ffmpeg.version)"
        "  URL:    $($spec.ffmpeg.url)"
        "  SHA256: $($spec.ffmpeg.sha256)"
        ""
        "PDFium $($spec.pdfium.version)"
        "  URL:    $($spec.pdfium.url)"
        "  SHA256: $($spec.pdfium.sha256)"
        ""
        "yt-dlp $($spec.yt_dlp.version) (Unlicense)"
        "  URL:    $($spec.yt_dlp.url)"
        "  SHA256: $($spec.yt_dlp.sha256)"
    ) | Set-Content (Join-Path $OutDir "licenses\SOURCES.txt") -Encoding UTF8
} finally {
    Remove-Item $scratch -Recurse -Force -ErrorAction SilentlyContinue
}
