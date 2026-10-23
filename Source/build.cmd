@echo off
rem Builds "Swiftlane Setup.exe" in the project folder:
rem   1. the app (Source\dist\Swiftlane)   2. payload.zip   3. the setup wizard with the payload inside.
rem Build scratch files go to %%TEMP%% and are deleted.
cd /d "%~dp0"
echo Checking libraries (and updating yt-dlp, since video sites change often)...
python -m pip install --user --disable-pip-version-check -q pyinstaller pywebview qrcode pillow pillow-heif ^
  sympy imageio-ffmpeg pypdfium2 cryptography
rem Send to contacts: pinned to an exact version (it handles the network connection between PCs).
python -m pip install --user --disable-pip-version-check -q --only-binary=:all: "iroh==1.2.0"
python -m pip install --user --disable-pip-version-check -q -U "yt-dlp[default]"
set "WORK=%TEMP%\Swiftlane-build"
set "PACK=%TEMP%\Swiftlane-payload"

rem A script with a syntax error silently breaks its whole page, so refuse to build with one (needs Node.js).
where node >nul 2>nul && (
  echo Checking the page scripts...
  for /r "%~dp0web" %%f in (*.js) do (
    node --check "%%f" || goto failed
  )
)

echo [1/3] Building the app...
python -m PyInstaller --onedir --windowed --noconfirm --log-level WARN --name Swiftlane ^
  --icon "%~dp0web\icon.ico" --add-data "%~dp0web;web" ^
  --collect-all yt_dlp_ejs --collect-data imageio_ffmpeg --collect-binaries imageio_ffmpeg --collect-binaries iroh --hidden-import iroh ^
  --distpath "%~dp0dist" --workpath "%WORK%" --specpath "%WORK%" "%~dp0swiftlane.py"
if errorlevel 1 goto failed

echo [2/3] Packing...
python "%~dp0setup\make_payload.py" "%~dp0." "%PACK%"
if errorlevel 1 goto failed

echo [3/3] Building the setup wizard...
python -m PyInstaller --onefile --windowed --uac-admin --noconfirm --log-level WARN --name "Swiftlane Setup" ^
  --icon "%~dp0web\icon.ico" --paths "%~dp0setup" ^
  --add-data "%PACK%\payload.zip;." --add-data "%PACK%\version.txt;." ^
  --add-data "%~dp0web\setup.html;web" --add-data "%~dp0web\setup;web\setup" --add-data "%~dp0web\css;web\css" --add-data "%~dp0web\js\i18n.js;web\js" ^
  --add-data "%~dp0web\icon-180.png;web" --add-data "%~dp0web\icon.ico;web" ^
  --distpath "%~dp0.." --workpath "%WORK%" --specpath "%WORK%" "%~dp0setup\setup.py"
if errorlevel 1 goto failed

rmdir /s /q "%WORK%" 2>nul
rmdir /s /q "%PACK%" 2>nul
rmdir /s /q "%~dp0dist" 2>nul
echo.
echo Done: "Swiftlane Setup.exe" is in the project folder.
if "%~1"=="" pause
exit /b 0

:failed
echo.
echo The build failed. See the messages above.
rmdir /s /q "%WORK%" 2>nul
if "%~1"=="" pause
exit /b 1
