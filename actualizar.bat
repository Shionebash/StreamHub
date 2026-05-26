@echo off
setlocal
cd /d "%~dp0"
title StreamHub - Actualizador Windows

echo.
echo  +==========================================+
echo  ^|      StreamHub Actualizador Windows     ^|
echo  +==========================================+
echo.

where git >nul 2>&1
if errorlevel 1 (
    echo  ERROR: git no encontrado en PATH.
    echo  Instala Git desde https://git-scm.com/download/win
    pause
    exit /b 1
)

git rev-parse --is-inside-work-tree >nul 2>&1
if errorlevel 1 (
    echo  ERROR: este directorio no parece ser un repositorio Git.
    pause
    exit /b 1
)

echo  Verificando cambios locales...
git diff --quiet --ignore-submodules --
if errorlevel 1 (
    echo  ERROR: hay cambios locales sin guardar en archivos versionados.
    echo  Guarda, revierte o commitea esos cambios antes de actualizar.
    pause
    exit /b 1
)

git diff --cached --quiet --ignore-submodules --
if errorlevel 1 (
    echo  ERROR: hay cambios preparados en Git.
    echo  Haz commit o quitalos del stage antes de actualizar.
    pause
    exit /b 1
)

echo.
echo  Descargando cambios desde origin...
git fetch origin
if errorlevel 1 (
    echo  ERROR: no se pudo ejecutar git fetch origin.
    pause
    exit /b 1
)

echo.
echo  Actualizando main con fast-forward...
git pull --ff-only origin main
if errorlevel 1 (
    echo  ERROR: no se pudo actualizar con fast-forward.
    echo  El repo local puede estar divergido. No se hizo reset ni se sobrescribieron cambios.
    pause
    exit /b 1
)

if exist ".venv\Scripts\python.exe" (
    echo.
    echo  Actualizando dependencias Python en .venv...
    ".venv\Scripts\python.exe" -m pip install --upgrade -r requirements.txt
    if errorlevel 1 (
        echo  ADVERTENCIA: no se pudieron actualizar las dependencias.
        echo  Puedes ejecutar install.bat para reparar la instalacion.
    )
) else (
    echo.
    echo  INFO: .venv no existe; se omite actualizacion de dependencias.
    echo  Ejecuta install.bat si necesitas instalar el entorno.
)

echo.
echo  OK: repositorio actualizado.
echo.
pause
