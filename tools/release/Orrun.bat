@echo off
setlocal EnableExtensions
cd /d "%~dp0"

if not exist "orrun.exe" (
    echo Orrun is not in this folder. Expected orrun.exe next to this script.
    echo Build a release with buildRelease.bat in the Orrun repo.
    pause
    exit /b 1
)

if not exist "assets\title\vista.png" (
    echo Game assets are missing. Expected an assets folder next to orrun.exe.
    echo Build a release with buildRelease.bat in the Orrun repo.
    pause
    exit /b 1
)

call :ensure_vcruntime
if errorlevel 1 (
    pause
    exit /b 1
)

orrun.exe %*
set "ERR=%ERRORLEVEL%"
if not "%ERR%"=="0" (
    echo.
    echo Orrun exited with error %ERR%.
    pause
)
exit /b %ERR%

:ensure_vcruntime
if exist "%SystemRoot%\System32\vcruntime140.dll" exit /b 0

echo Visual C++ runtime is not installed. Downloading it now...
set "REDIST=%TEMP%\orrun-vc_redist.x64.exe"
powershell -NoProfile -Command "Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vc_redist.x64.exe' -OutFile $env:TEMP\orrun-vc_redist.x64.exe"
if errorlevel 1 (
    echo Failed to download the Visual C++ runtime from Microsoft.
    echo Install "Microsoft Visual C++ Redistributable for Visual Studio 2015-2022" x64, then run this again.
    exit /b 1
)
if not exist "%REDIST%" (
    echo Download finished but %REDIST% is missing.
    exit /b 1
)

echo Installing Visual C++ runtime. Accept the Windows prompt if it appears.
"%REDIST%" /install /passive /norestart
if errorlevel 1 (
    echo Visual C++ runtime install failed.
    echo Install "Microsoft Visual C++ Redistributable for Visual Studio 2015-2022" x64, then run this again.
    exit /b 1
)
if not exist "%SystemRoot%\System32\vcruntime140.dll" (
    echo Visual C++ runtime install finished but vcruntime140.dll is still missing.
    exit /b 1
)
echo Visual C++ runtime is ready.
exit /b 0
