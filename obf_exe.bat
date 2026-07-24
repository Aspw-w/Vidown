@echo off
setlocal EnableExtensions

cd /d "%~dp0"

set "APP_NAME=Vidown"
set "ENTRY_FILE=app.py"
set "ICON_FILE=icon.ico"
set "OBF_DIR=build\obf"
set "OBF_ENTRY=%OBF_DIR%\app.py"
set "ICON_PATH=%CD%\%ICON_FILE%"

if not exist "%ENTRY_FILE%" (
    echo [ERROR] %ENTRY_FILE% was not found.
    exit /b 1
)

if not exist "%ICON_FILE%" (
    echo [ERROR] %ICON_FILE% was not found.
    exit /b 1
)

set "PYTHON_CMD="
where py >nul 2>nul
if not errorlevel 1 (
    set "PYTHON_CMD=py -3"
) else (
    where python >nul 2>nul
    if errorlevel 1 (
        echo [ERROR] Python was not found in PATH.
        exit /b 1
    )
    set "PYTHON_CMD=python"
)

echo [1/5] Installing runtime and build dependencies...
%PYTHON_CMD% -m pip install --upgrade -r requirements.txt pyinstaller python-minifier
if errorlevel 1 goto :error

echo [2/5] Cleaning previous build artifacts...
if exist build rmdir /s /q build
if exist dist rmdir /s /q dist
if exist "%APP_NAME%.spec" del /q "%APP_NAME%.spec"
mkdir "%OBF_DIR%"

echo [3/5] Obfuscating %ENTRY_FILE%...
%PYTHON_CMD% -c "from pathlib import Path; import python_minifier; src = Path(r'%ENTRY_FILE%').read_text(encoding='utf-8'); out = python_minifier.minify(src, filename=r'%ENTRY_FILE%', rename_globals=True); Path(r'%OBF_ENTRY%').write_text(out, encoding='utf-8')"
if errorlevel 1 goto :error

echo [4/5] Building one-file executable...
%PYTHON_CMD% -m PyInstaller --onefile --windowed --name %APP_NAME% --icon "%ICON_PATH%" --add-data "%ICON_PATH%;." --collect-all customtkinter --collect-all yt_dlp --clean --noconfirm --distpath dist --workpath build\pyinstaller --specpath build\spec "%OBF_ENTRY%"
if errorlevel 1 goto :error

if not exist "dist\%APP_NAME%.exe" (
    echo [ERROR] Build completed but dist\%APP_NAME%.exe was not found.
    exit /b 1
)

echo [5/5] Completed: dist\%APP_NAME%.exe
exit /b 0

:error
echo [ERROR] Build failed.
exit /b 1
