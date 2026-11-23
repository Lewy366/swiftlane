@echo off
rem Builds "Swiftlane Setup.exe" from the Rust version (Rust/ + pinned SHA-256-verified helpers).
title Build Swiftlane Setup (Rust)
where pwsh >nul 2>nul && (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0Rust\pack\build-setup.ps1" %*
  exit /b %errorlevel%
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Rust\pack\build-setup.ps1" %*
exit /b %errorlevel%
