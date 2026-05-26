@echo off
setlocal enabledelayedexpansion
cd /d "%~dp0"
title StreamHub - Instalador Windows

echo.
echo  +==========================================+
echo  ^|      StreamHub Instalador Windows       ^|
echo  +==========================================+
echo.

echo  [1/5] Verificando Python 3...
set "PY_BASE="
where py >nul 2>&1
if not errorlevel 1 (
    py -3 --version >nul 2>&1
    if not errorlevel 1 set "PY_BASE=py -3"
)
if "%PY_BASE%"=="" (
    python --version >nul 2>&1
    if not errorlevel 1 set "PY_BASE=python"
)
if "%PY_BASE%"=="" (
    echo  ERROR: Python 3 no encontrado.
    echo  Instala Python 3 desde https://www.python.org/downloads/
    echo  Asegurate de marcar "Add Python to PATH" durante la instalacion.
    pause
    exit /b 1
)
for /f "tokens=*" %%v in ('%PY_BASE% --version 2^>^&1') do set "PY_VER=%%v"
echo  OK: !PY_VER!

echo.
echo  [2/5] Creando entorno virtual local...
if not exist ".venv\Scripts\python.exe" (
    %PY_BASE% -m venv .venv
    if errorlevel 1 (
        echo  ERROR: No se pudo crear .venv.
        pause
        exit /b 1
    )
)
set "VENV_DIR=%CD%\.venv"
set "PYTHON_EXE=%VENV_DIR%\Scripts\python.exe"
set "PATH=%VENV_DIR%\Scripts;%PATH%"
echo  OK: .venv listo

echo.
echo  [3/5] Instalando dependencias Python en .venv...
"%PYTHON_EXE%" -m ensurepip --upgrade >nul 2>&1
"%PYTHON_EXE%" -m pip install --upgrade pip --quiet
if errorlevel 1 echo  ADVERTENCIA: No se pudo actualizar pip, continuando...
"%PYTHON_EXE%" -m pip install --upgrade -r requirements.txt --quiet
if errorlevel 1 (
    echo  ERROR: No se pudieron instalar las dependencias de requirements.txt.
    pause
    exit /b 1
)
echo  OK: requirements.txt instalado

echo.
echo  [4/5] Verificando ffmpeg...
ffmpeg -version >nul 2>&1
if not errorlevel 1 (
    echo  OK: ffmpeg encontrado en PATH
) else (
    winget --version >nul 2>&1
    if not errorlevel 1 (
        echo  Instalando ffmpeg via winget...
        winget install --id Gyan.FFmpeg -e --silent
    ) else (
        choco --version >nul 2>&1
        if not errorlevel 1 (
            echo  Instalando ffmpeg via choco...
            choco install ffmpeg -y --no-progress
        ) else (
            echo  ADVERTENCIA: ffmpeg no encontrado.
            echo  Descarga manual: https://www.gyan.dev/ffmpeg/builds/
        )
    )
)

echo.
echo  [5/5] Detectando reproductores...
set "MPV_PATH=mpv"
set "VLC_PATH=vlc"
set "GP_PATH=gridplayer"

mpv --version >nul 2>&1
if not errorlevel 1 (
    echo  OK: mpv encontrado en PATH
) else if exist "C:\Program Files\mpv\mpv.exe" (
    set "MPV_PATH=C:\Program Files\mpv\mpv.exe"
    echo  OK: mpv encontrado en Program Files
) else if exist "%LOCALAPPDATA%\Programs\mpv\mpv.exe" (
    set "MPV_PATH=%LOCALAPPDATA%\Programs\mpv\mpv.exe"
    echo  OK: mpv encontrado en AppData
) else (
    echo  INFO: mpv no encontrado. Recomendado: winget install mpv.mpv
)

vlc --version >nul 2>&1
if not errorlevel 1 (
    echo  OK: vlc encontrado en PATH
) else if exist "C:\Program Files\VideoLAN\VLC\vlc.exe" (
    set "VLC_PATH=C:\Program Files\VideoLAN\VLC\vlc.exe"
    echo  OK: vlc encontrado en Program Files
) else if exist "C:\Program Files (x86)\VideoLAN\VLC\vlc.exe" (
    set "VLC_PATH=C:\Program Files (x86)\VideoLAN\VLC\vlc.exe"
    echo  OK: vlc encontrado en Program Files x86
) else (
    echo  INFO: vlc no encontrado. Opcional: winget install VideoLAN.VLC
)

gridplayer --version >nul 2>&1
if not errorlevel 1 (
    echo  OK: gridplayer encontrado en PATH
) else if exist "C:\Program Files\GridPlayer\GridPlayer.exe" (
    set "GP_PATH=C:\Program Files\GridPlayer\GridPlayer.exe"
    echo  OK: gridplayer encontrado en Program Files
) else if exist "%LOCALAPPDATA%\Programs\GridPlayer\GridPlayer.exe" (
    set "GP_PATH=%LOCALAPPDATA%\Programs\GridPlayer\GridPlayer.exe"
    echo  OK: gridplayer encontrado en AppData
) else (
    echo  INFO: gridplayer no encontrado. Es opcional.
)

echo.
echo  Actualizando config.json local...
"%PYTHON_EXE%" -c "import json, os, sys; p='config.json'; d={'player':'mpv','mpv':sys.argv[1],'vlc':sys.argv[2],'gp':sys.argv[3],'ff':'ffmpeg','sl':'streamlink','quality':'480p','w':1920,'h':1080,'hwaccel':False,'hwgpu':'nvidia','vlcFfmpeg':False,'textOnly':False,'accentColor':'#e0553a','clientId':'','token':'','miningEnabled':False}; k=('clientId','client_id','token','refresh_token','player','quality','w','h','hwaccel','hwgpu','vlcFfmpeg','textOnly','accentColor','miningEnabled'); e={}; exec('import json, os\ntry:\n    e=json.load(open(p, encoding=\"utf-8\")) if os.path.exists(p) else {}\nexcept Exception:\n    e={}\nfor x in k:\n    if x in e:\n        d[x]=e[x]\njson.dump(d, open(p, \"w\", encoding=\"utf-8\"), indent=2, ensure_ascii=False)\nprint(\"  OK: config.json actualizado\")')" "%MPV_PATH%" "%VLC_PATH%" "%GP_PATH%"
if errorlevel 1 (
    echo  ERROR: No se pudo actualizar config.json.
    pause
    exit /b 1
)

echo.
echo  +==========================================+
echo  ^|            RESUMEN INSTALACION           ^|
echo  +==========================================+
"%VENV_DIR%\Scripts\streamlink.exe" --version >nul 2>&1 && echo  streamlink : OK ^(.venv^) || echo  streamlink : FALTA
ffmpeg -version >nul 2>&1 && echo  ffmpeg     : OK || echo  ffmpeg     : FALTA
if "%MPV_PATH%"=="mpv" (
    mpv --version >nul 2>&1 && echo  mpv        : OK || echo  mpv        : FALTA
) else (
    echo  mpv        : %MPV_PATH%
)
if "%VLC_PATH%"=="vlc" (
    vlc --version >nul 2>&1 && echo  vlc        : OK || echo  vlc        : FALTA ^(opcional^)
) else (
    echo  vlc        : %VLC_PATH%
)
echo.
echo  Para iniciar StreamHub ejecuta: iniciar.bat
echo.
pause
