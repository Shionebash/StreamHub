#!/usr/bin/env bash
# StreamHub - Linux updater
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" && pwd -P)"
cd "$SCRIPT_DIR"

echo ""
echo " +==========================================+"
echo " |      StreamHub Actualizador Linux       |"
echo " +==========================================+"
echo ""

if ! command -v git >/dev/null 2>&1; then
    echo " ERROR: git no encontrado en PATH."
    echo " Instala Git con tu gestor de paquetes."
    exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo " ERROR: este directorio no parece ser un repositorio Git."
    exit 1
fi

echo " Verificando cambios locales..."
if ! git diff --quiet --ignore-submodules --; then
    echo " ERROR: hay cambios locales sin guardar en archivos versionados."
    echo " Guarda, revierte o commitea esos cambios antes de actualizar."
    exit 1
fi

if ! git diff --cached --quiet --ignore-submodules --; then
    echo " ERROR: hay cambios preparados en Git."
    echo " Haz commit o quitalos del stage antes de actualizar."
    exit 1
fi

echo ""
echo " Descargando cambios desde origin..."
git fetch origin

echo ""
echo " Actualizando main con fast-forward..."
if ! git pull --ff-only origin main; then
    echo " ERROR: no se pudo actualizar con fast-forward."
    echo " El repo local puede estar divergido. No se hizo reset ni se sobrescribieron cambios."
    exit 1
fi

if [ -x ".venv/bin/python" ]; then
    echo ""
    echo " Actualizando dependencias Python en .venv..."
    if ! ".venv/bin/python" -m pip install --upgrade -r requirements.txt; then
        echo " ADVERTENCIA: no se pudieron actualizar las dependencias."
        echo " Puedes ejecutar bash install.sh para reparar la instalacion."
    fi
else
    echo ""
    echo " INFO: .venv no existe; se omite actualizacion de dependencias."
    echo " Ejecuta bash install.sh si necesitas instalar el entorno."
fi

chmod +x iniciar.sh install.sh actualizar.sh 2>/dev/null || true

echo ""
echo " OK: repositorio actualizado."
echo ""
