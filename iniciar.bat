@echo off
setlocal
cd /d "%~dp0"

set "VENV_DIR=%CD%\.venv"
if exist "%VENV_DIR%\Scripts\python.exe" (
    set "PATH=%VENV_DIR%\Scripts;%PATH%"
    set "PYTHON_EXE=%VENV_DIR%\Scripts\python.exe"
) else (
    where py >nul 2>&1
    if not errorlevel 1 (
        set "PYTHON_EXE=py -3"
    ) else (
        set "PYTHON_EXE=python"
    )
)

start "StreamHub Server" %PYTHON_EXE% server.py
timeout /t 1 /nobreak >nul
start "" "http://localhost:8080/StreamHub.html"
