@echo off
setlocal EnableExtensions
cd /d "%~dp0"
set "ROOT=%CD%"
set "OUT=%ROOT%\release"
set "ENGINE=%ROOT%\..\Engine\engine\Cargo.toml"
set "MODULAR=%ROOT%\..\Modular\modular\Cargo.toml"

echo.
echo === Orrun release ===
echo.

if not exist "%ENGINE%" (
    echo Engine crate not found at:
    echo   %ENGINE%
    echo This repo expects the Engine checkout as a sibling folder.
    exit /b 1
)
if not exist "%MODULAR%" (
    echo Modular crate not found at:
    echo   %MODULAR%
    echo This repo expects the Modular checkout as a sibling folder.
    exit /b 1
)
if not exist "%ROOT%\orrun\assets\title\vista.png" (
    echo Game assets are missing. Expected orrun\assets\title\vista.png
    exit /b 1
)

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
call :ensure_cargo
if errorlevel 1 exit /b 1

echo Building orrun in release mode...
cargo build --release -p orrun --bin orrun %*
if errorlevel 1 (
    echo cargo build --release failed.
    exit /b 1
)
if not exist "%ROOT%\target\release\orrun.exe" (
    echo Build reported success but target\release\orrun.exe is missing.
    exit /b 1
)

echo.
echo Packing %OUT%
if exist "%OUT%" rmdir /s /q "%OUT%"
mkdir "%OUT%"
if errorlevel 1 (
    echo Could not create %OUT%
    exit /b 1
)

copy /Y "%ROOT%\target\release\orrun.exe" "%OUT%\orrun.exe" >nul
if errorlevel 1 (
    echo Failed to copy orrun.exe
    exit /b 1
)
copy /Y "%ROOT%\tools\release\Orrun.bat" "%OUT%\Orrun.bat" >nul
if errorlevel 1 (
    echo Failed to copy Orrun.bat
    exit /b 1
)

robocopy "%ROOT%\orrun\assets" "%OUT%\assets" /E /R:2 /W:1 /NFL /NDL /NJH /NJS /NC /NS /NP
if errorlevel 8 (
    echo Failed to copy orrun\assets into the release folder.
    exit /b 1
)
if not exist "%OUT%\assets\title\vista.png" (
    echo Assets copy finished but assets\title\vista.png is missing.
    exit /b 1
)

(
    echo Orrun
    echo.
    echo Double-click Orrun.bat to start.
    echo The first launch installs the Visual C++ runtime if Windows does not already have it.
    echo A DirectX 12 GPU is required ^(Windows 10 or later^).
) > "%OUT%\README.txt"

echo.
echo Release is ready:
echo   %OUT%\Orrun.bat
echo.
exit /b 0

:ensure_cargo
where cargo >nul 2>&1
if not errorlevel 1 (
    cargo --version
    exit /b 0
)

echo cargo is not on PATH. Installing Rust rustup, stable MSVC...
set "INIT=%TEMP%\orrun-rustup-init.exe"
powershell -NoProfile -Command "Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $env:TEMP\orrun-rustup-init.exe"
if errorlevel 1 (
    echo Failed to download rustup-init from https://win.rustup.rs/x86_64
    exit /b 1
)
if not exist "%INIT%" (
    echo Download finished but %INIT% is missing.
    exit /b 1
)

"%INIT%" -y --default-toolchain stable --default-host x86_64-pc-windows-msvc
if errorlevel 1 (
    echo rustup-init failed.
    exit /b 1
)

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
if exist "%USERPROFILE%\.cargo\env.bat" call "%USERPROFILE%\.cargo\env.bat"

where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust installed but cargo is still not on PATH. Open a new terminal and run this script again.
    exit /b 1
)
cargo --version
exit /b 0
