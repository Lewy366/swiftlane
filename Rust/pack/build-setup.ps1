# Builds "Swiftlane Setup.exe" for the Rust version of Swiftlane.
#
# Usage:
#   pwsh -File Rust/pack/build-setup.ps1                       # full local Rust build + pack + Setup.exe
#   pwsh -File Rust/pack/build-setup.ps1 -FromDir path\to\out  # repack an already-built CI folder into Setup.exe
#   pwsh -File Rust/pack/build-setup.ps1 -SkipTests            # skip cargo test during local build
#   pwsh -File Rust/pack/build-setup.ps1 -Output "Swiftlane Rust Setup.exe"
#
# Build scratch files live in %TEMP% and %LOCALAPPDATA% (never in Rust\target or OneDrive).
param(
    [string]$FromDir = "",
    [string]$Output = "",
    [switch]$SkipTests,
    [switch]$NoPause
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$RustDir = Join-Path $RepoRoot "Rust"
$SourceDir = Join-Path $RepoRoot "Source"
$PackDir = $PSScriptRoot

$runId = [guid]::NewGuid().ToString("N").Substring(0, 8)
$Work = Join-Path ([IO.Path]::GetTempPath()) "Swiftlane-rust-build-$runId"
$Stage = Join-Path $Work "app"
$PackOut = Join-Path $Work "payload"
$PyWork = Join-Path $Work "pyi"

function Smoke-Check([string]$AppDir) {
    $required = @(
        "Swiftlane.exe",
        "ffmpeg.exe",
        "ffprobe.exe",
        "pdfium.dll",
        "yt-dlp.exe",
        "toolkit\calc.py",
        "uninstall.ps1",
        "airplay-install.ps1",
        "licenses\SOURCES.txt"
    )
    foreach ($rel in $required) {
        $p = Join-Path $AppDir $rel
        if (-not (Test-Path $p) -or (Get-Item $p).Length -eq 0) {
            throw "Missing or empty bundled file: $rel"
        }
    }

    $oldPath = $env:PATH
    try {
        $env:PATH = "C:\Windows\System32;C:\Windows"
        $ffOut = & (Join-Path $AppDir "ffmpeg.exe") -version
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg.exe failed (exit $LASTEXITCODE)" }
        $ffOut | Select-Object -First 1

        $fpOut = & (Join-Path $AppDir "ffprobe.exe") -version
        if ($LASTEXITCODE -ne 0) { throw "ffprobe.exe failed (exit $LASTEXITCODE)" }
        $fpOut | Select-Object -First 1

        & (Join-Path $AppDir "yt-dlp.exe") --version
        if ($LASTEXITCODE -ne 0) { throw "yt-dlp.exe failed (exit $LASTEXITCODE)" }
    } finally {
        $env:PATH = $oldPath
    }

    $dllPath = (Resolve-Path (Join-Path $AppDir "pdfium.dll")).Path
    if (-not ("SwiftlaneDllCheck" -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class SwiftlaneDllCheck {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryW(string path);
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool FreeLibrary(IntPtr h);
}
'@
    }
    $h = [SwiftlaneDllCheck]::LoadLibraryW($dllPath)
    if ($h -eq [IntPtr]::Zero) {
        $err = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "pdfium.dll failed to load (Win32 error $err)"
    }
    [void][SwiftlaneDllCheck]::FreeLibrary($h)
    Write-Host "Smoke-check passed (ffmpeg, ffprobe, yt-dlp, pdfium.dll)."
}

try {
    New-Item -ItemType Directory -Force -Path $Work, $PackOut, $PyWork | Out-Null

    Write-Host "[1/5] Checking versions and web scripts..."
    & python (Join-Path $PackDir "set-version.py") --check
    if ($LASTEXITCODE -ne 0) { throw "Rust version files are out of sync. Run Rust/pack/set-version.py <X.Y.Z>." }

    if (Get-Command node -ErrorAction SilentlyContinue) {
        Get-ChildItem (Join-Path $SourceDir "web") -Recurse -Filter "*.js" | ForEach-Object {
            & node --check $_.FullName
            if ($LASTEXITCODE -ne 0) { throw "JavaScript syntax error in $($_.FullName)" }
        }
    }

    $appDir = $Stage
    if ($FromDir) {
        $appDir = (Resolve-Path $FromDir).Path
        Write-Host "[2/5] Using pre-built Rust folder: $appDir"
    } else {
        Write-Host "[2/5] Fetching pinned helpers and building Rust app..."
        New-Item -ItemType Directory -Force -Path $Stage, (Join-Path $Stage "toolkit") | Out-Null
        & (Join-Path $PackDir "fetch-helpers.ps1") -OutDir $Stage

        if (Test-Path (Join-Path $env:USERPROFILE ".cargo\bin")) {
            $env:PATH = (Join-Path $env:USERPROFILE ".cargo\bin") + ";" + $env:PATH
        }
        $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $env:LOCALAPPDATA "swiftlane-cargo-target" }
        $env:CARGO_TARGET_DIR = $targetDir

        Push-Location $RustDir
        try {
            if (-not $SkipTests) {
                Write-Host "Running cargo test --locked (excluding local AppData Temp share test)..."
                & cargo test --locked -- --skip share::tests::download_counting
                if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }
            }
            Write-Host "Building Swiftlane.exe (release)..."
            & cargo build --release --locked
            if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }
        } finally {
            Pop-Location
        }

        Copy-Item (Join-Path $targetDir "release\swiftlane.exe") (Join-Path $Stage "Swiftlane.exe") -Force
        Copy-Item (Join-Path $SourceDir "uninstall.ps1") (Join-Path $Stage "uninstall.ps1") -Force
        Copy-Item (Join-Path $SourceDir "airplay-install.ps1") (Join-Path $Stage "airplay-install.ps1") -Force
        Copy-Item (Join-Path $SourceDir "toolkit\calc.py") (Join-Path $Stage "toolkit\calc.py") -Force
    }

    Write-Host "[3/5] Smoke-checking assembled Rust folder..."
    Smoke-Check $appDir

    Write-Host "[4/5] Packing payload.zip..."
    & python (Join-Path $PackDir "make-payload.py") $appDir $PackOut
    if ($LASTEXITCODE -ne 0) { throw "make-payload.py failed" }

    Write-Host "[5/5] Building Setup wizard executable..."
    & python -m pip install --user --disable-pip-version-check -q pyinstaller pywebview
    if ($LASTEXITCODE -ne 0) { throw "pip install pyinstaller/pywebview failed" }

    $outPath = if ($Output) {
        if ([IO.Path]::IsPathRooted($Output)) { $Output } else { Join-Path $RepoRoot $Output }
    } else {
        Join-Path $RepoRoot "Swiftlane Setup.exe"
    }
    $outDir = Split-Path -Parent $outPath
    $outBase = [IO.Path]::GetFileNameWithoutExtension($outPath)
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null

    $web = Join-Path $SourceDir "web"
    & python -m PyInstaller --onefile --windowed --uac-admin --noconfirm --log-level WARN `
        --name $outBase `
        --icon (Join-Path $web "icon.ico") `
        --paths (Join-Path $SourceDir "setup") `
        --add-data "$PackOut\payload.zip;." `
        --add-data "$PackOut\version.txt;." `
        --add-data "$web\setup.html;web" `
        --add-data "$web\setup;web\setup" `
        --add-data "$web\css;web\css" `
        --add-data "$web\js\i18n.js;web\js" `
        --add-data "$web\icon-180.png;web" `
        --add-data "$web\icon.ico;web" `
        --distpath $outDir `
        --workpath $PyWork `
        --specpath $PyWork `
        (Join-Path $SourceDir "setup\setup.py")
    if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed building Setup wizard" }

    $sizeMb = [Math]::Round((Get-Item $outPath).Length / 1MB, 1)
    Write-Host ""
    Write-Host "Done: $outPath ($sizeMb MB)"
} finally {
    Remove-Item $Work -Recurse -Force -ErrorAction SilentlyContinue
}

if (-not $NoPause) {
    Read-Host "Press Enter to close" | Out-Null
}
