#!/usr/bin/env bash
# StreamHub - Linux installer
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-$0}")" && pwd -P)"
cd "$SCRIPT_DIR"

echo ""
echo " +==========================================+"
echo " |      StreamHub Instalador Linux         |"
echo " +==========================================+"
echo ""

PKG_MANAGER=""
INSTALL_CMD=""

if command -v apt >/dev/null 2>&1; then
    PKG_MANAGER="apt"
    INSTALL_CMD="sudo apt install -y"
elif command -v apt-get >/dev/null 2>&1; then
    PKG_MANAGER="apt-get"
    INSTALL_CMD="sudo apt-get install -y"
elif command -v dnf >/dev/null 2>&1; then
    PKG_MANAGER="dnf"
    INSTALL_CMD="sudo dnf install -y"
elif command -v pacman >/dev/null 2>&1; then
    PKG_MANAGER="pacman"
    INSTALL_CMD="sudo pacman -S --noconfirm"
elif command -v zypper >/dev/null 2>&1; then
    PKG_MANAGER="zypper"
    INSTALL_CMD="sudo zypper install -y"
fi

echo " Gestor de paquetes detectado: ${PKG_MANAGER:-ninguno}"
echo ""

install_pkg() {
    if [ -z "$PKG_MANAGER" ]; then
        return 1
    fi
    case "$PKG_MANAGER" in
        apt|apt-get|dnf|zypper) $INSTALL_CMD "$@" ;;
        pacman) sudo pacman -S --noconfirm "$@" ;;
    esac
}

echo " [1/5] Verificando Python 3..."
if ! command -v python3 >/dev/null 2>&1; then
    echo " ERROR: python3 no encontrado."
    echo " Instala Python 3 con tu gestor de paquetes."
    exit 1
fi
echo " OK: $(python3 --version 2>&1)"

echo ""
echo " [2/5] Creando entorno virtual local..."
if [ ! -d ".venv" ]; then
    if ! python3 -m venv .venv >/dev/null 2>&1; then
        echo " python3-venv no esta disponible. Intentando instalarlo..."
        case "$PKG_MANAGER" in
            apt|apt-get) install_pkg python3-venv python3-pip ;;
            dnf)         install_pkg python3 python3-pip ;;
            pacman)      install_pkg python python-pip ;;
            zypper)      install_pkg python3 python3-pip ;;
            *)           echo " Instala el modulo venv de Python e intenta de nuevo."; exit 1 ;;
        esac
        python3 -m venv .venv
    fi
fi
PYTHON="$SCRIPT_DIR/.venv/bin/python"
PIP=("$PYTHON" -m pip)
echo " OK: .venv listo"

echo ""
echo " [3/5] Instalando dependencias Python en .venv..."
"$PYTHON" -m ensurepip --upgrade >/dev/null 2>&1 || true
"${PIP[@]}" install --upgrade pip --quiet
"${PIP[@]}" install --upgrade -r requirements.txt --quiet
echo " OK: requirements.txt instalado"

echo ""
echo " [4/5] Verificando ffmpeg..."
if command -v ffmpeg >/dev/null 2>&1; then
    echo " OK: ffmpeg encontrado en PATH"
elif install_pkg ffmpeg; then
    echo " OK: ffmpeg instalado"
else
    echo " ADVERTENCIA: instala ffmpeg manualmente si usaras grid con composicion."
fi

echo ""
echo " [5/5] Verificando reproductores..."
if command -v mpv >/dev/null 2>&1; then
    echo " OK: mpv encontrado en PATH"
elif install_pkg mpv; then
    echo " OK: mpv instalado"
else
    echo " ADVERTENCIA: mpv no encontrado. Instala mpv o configura otro reproductor."
fi

if command -v vlc >/dev/null 2>&1; then
    echo " OK: vlc encontrado en PATH"
elif [ -t 0 ]; then
    read -r -p " Instalar VLC? (s/N): " INSTALL_VLC
    INSTALL_VLC="${INSTALL_VLC:-n}"
    if [[ "$INSTALL_VLC" =~ ^[sS]$ ]]; then
        install_pkg vlc && echo " OK: vlc instalado" || echo " ADVERTENCIA: no se pudo instalar vlc"
    else
        echo " INFO: VLC omitido. Es opcional."
    fi
else
    echo " INFO: VLC no encontrado. Es opcional."
fi

chmod +x iniciar.sh 2>/dev/null || true

echo ""
echo " Actualizando config.json local..."
"$PYTHON" - <<'PYEOF'
import json
import os

config_file = "config.json"
defaults = {
    "player": "mpv",
    "mpv": "mpv",
    "vlc": "vlc",
    "gp": "gridplayer",
    "ff": "ffmpeg",
    "sl": "streamlink",
    "quality": "480p",
    "w": 1920,
    "h": 1080,
    "hwaccel": False,
    "hwgpu": "nvidia",
    "vlcFfmpeg": False,
    "textOnly": False,
    "accentColor": "#e0553a",
    "clientId": "",
    "token": "",
    "miningEnabled": False,
}

if os.path.exists(config_file):
    try:
        with open(config_file, "r", encoding="utf-8") as f:
            existing = json.load(f)
        for key in (
            "clientId", "client_id", "token", "refresh_token", "player",
            "quality", "w", "h", "hwaccel", "hwgpu", "vlcFfmpeg",
            "textOnly", "accentColor", "miningEnabled"
        ):
            if key in existing:
                defaults[key] = existing[key]
    except Exception:
        pass

with open(config_file, "w", encoding="utf-8") as f:
    json.dump(defaults, f, indent=2, ensure_ascii=False)
print(" OK: config.json actualizado")
PYEOF

echo ""
echo " +==========================================+"
echo " |            RESUMEN INSTALACION           |"
echo " +==========================================+"

check_cmd() {
    if command -v "$1" >/dev/null 2>&1; then
        echo "  $1: OK"
    else
        echo "  $1: FALTA ($2)"
    fi
}

if .venv/bin/streamlink --version >/dev/null 2>&1; then
    echo "  streamlink: OK (.venv)"
else
    echo "  streamlink: FALTA (.venv)"
fi
check_cmd ffmpeg "instala ffmpeg"
check_cmd mpv "instala mpv"
check_cmd vlc "opcional"

echo ""
echo " Para iniciar StreamHub:"
echo "   bash iniciar.sh"
echo ""
