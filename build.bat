@echo off
setlocal
cd /d "%~dp0"

set "BUNDLED=%~dp0bundled"
set "DOWNLOADS=%~dp0target\bundle-downloads"
set "DIST=%~dp0dist"

set "YTDLP_URL=https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp.exe"
set "FFMPEG_URL=https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl-shared.zip"
set "DENO_URL=https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip"

where cargo >nul 2>nul || (echo [ERROR] cargo not found. Install Rust from https://rustup.rs/ & exit /b 1)
where curl >nul 2>nul || (echo [ERROR] curl not found. & exit /b 1)
where tar >nul 2>nul || (echo [ERROR] tar not found. & exit /b 1)

if /i "%~1"=="update" if exist "%BUNDLED%" rmdir /s /q "%BUNDLED%"
if not exist "%BUNDLED%" mkdir "%BUNDLED%"
if exist "%DOWNLOADS%" rmdir /s /q "%DOWNLOADS%"
mkdir "%DOWNLOADS%"

echo [1/4] Downloading yt-dlp nightly...
curl -fL --retry 3 -o "%DOWNLOADS%\yt-dlp.exe" "%YTDLP_URL%" || goto :download_failed
move /y "%DOWNLOADS%\yt-dlp.exe" "%BUNDLED%\yt-dlp.exe" >nul || goto :error

echo [2/4] Preparing ffmpeg...
if exist "%BUNDLED%\ffmpeg.exe" (
    echo       cached
) else (
    curl -fL --retry 3 -o "%DOWNLOADS%\ffmpeg.zip" "%FFMPEG_URL%" || goto :download_failed
    mkdir "%DOWNLOADS%\ffmpeg"
    tar -xf "%DOWNLOADS%\ffmpeg.zip" -C "%DOWNLOADS%\ffmpeg" || goto :error
    for /d %%D in ("%DOWNLOADS%\ffmpeg\*") do (
        copy /y "%%D\bin\*.dll" "%BUNDLED%\" >nul || goto :error
        copy /y "%%D\bin\ffprobe.exe" "%BUNDLED%\" >nul || goto :error
        copy /y "%%D\bin\ffmpeg.exe" "%BUNDLED%\" >nul || goto :error
    )
)
if not exist "%BUNDLED%\ffmpeg.exe" goto :error

echo [3/4] Preparing Deno...
if exist "%BUNDLED%\deno.exe" (
    echo       cached
) else (
    curl -fL --retry 3 -o "%DOWNLOADS%\deno.zip" "%DENO_URL%" || goto :download_failed
    tar -xf "%DOWNLOADS%\deno.zip" -C "%BUNDLED%" deno.exe || goto :error
)
if not exist "%BUNDLED%\deno.exe" goto :error

rmdir /s /q "%DOWNLOADS%"

echo [4/4] Building Vidown.exe (compressing the bundled tools takes a while)...
cargo build --release || goto :error

if not exist "%DIST%" mkdir "%DIST%"
copy /y "%~dp0target\release\Vidown.exe" "%DIST%\Vidown.exe" >nul || goto :error

echo.
echo Done: %DIST%\Vidown.exe
exit /b 0

:download_failed
echo [ERROR] Download failed. Check your network connection and try again.
exit /b 1

:error
echo [ERROR] Build failed.
exit /b 1
