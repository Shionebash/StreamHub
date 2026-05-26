# StreamHub

StreamHub es una app local para abrir y organizar multiples streams de Twitch con bajo consumo. Usa Streamlink para resolver streams y un reproductor local como mpv, VLC o GridPlayer para reproducirlos.

La app corre en tu maquina, guarda la configuracion localmente y evita exponer tokens en el frontend. Es ideal para multistream, seguimiento de canales en vivo, grillas de varios canales, drops y puntos de canal.

## Caracteristicas

- Cola de canales con controles por canal.
- Reproduccion individual con mpv o VLC.
- Modo grid con mpv, VLC mosaic o GridPlayer.
- Modo audio-only por canal.
- Favoritos, seguidos en vivo, exploracion por categorias y busqueda.
- Cache local de previews y estado en vivo.
- Panel de settings y ajustes visuales.
- Token Twitch guardado de forma segura con keyring o cifrado local.
- Soporte para drops, channel points y PubSub cuando hay token configurado.

## Requisitos

- Python 3.10 o superior.
- Streamlink, instalado automaticamente en `.venv`.
- `ffmpeg`, requerido para grids con composicion.
- `mpv`, recomendado como reproductor principal.
- VLC, opcional.
- GridPlayer, opcional.

En Windows puedes instalar herramientas con `winget`:

```powershell
winget install Python.Python.3.13
winget install mpv.mpv
winget install Gyan.FFmpeg
winget install VideoLAN.VLC
```

En Linux usa el gestor de paquetes de tu distro, por ejemplo:

```bash
sudo apt install python3 python3-venv ffmpeg mpv vlc
```

## Instalacion

### Windows

Ejecuta:

```bat
install.bat
```

El instalador crea `.venv`, instala dependencias de Python desde `requirements.txt`, detecta reproductores disponibles y genera `config.json` sin borrar credenciales existentes.

### Linux

Ejecuta:

```bash
bash install.sh
```

El instalador crea `.venv`, instala dependencias y trata de instalar herramientas del sistema cuando el gestor de paquetes esta disponible.

## Inicio

### Windows

```bat
iniciar.bat
```

### Linux

```bash
bash iniciar.sh
```

La app queda disponible en:

```text
http://localhost:8080/StreamHub.html
```

Tambien puedes iniciar manualmente:

```bash
python server.py
```

## Actualizacion

Para actualizar el repo desde `origin/main` sin pisar cambios locales:

### Windows

```bat
actualizar.bat
```

### Linux

```bash
bash actualizar.sh
```

El actualizador se detiene si detecta cambios locales en archivos versionados. No hace `git reset` ni sobrescribe `config.json`, logs, runtime ni credenciales locales.

## Configuracion

La configuracion principal vive en `config.json`. Puedes partir de `config.example.json` si necesitas reconstruirla.

Campos principales:

- `player`: reproductor por defecto. Valores: `mpv`, `vlc`, `gridplayer`.
- `quality`: calidad de stream para Streamlink. Ejemplos: `best`, `720p`, `480p`, `audio_only`.
- `mpv`, `vlc`, `gp`, `ff`, `sl`: rutas o comandos para mpv, VLC, GridPlayer, ffmpeg y Streamlink.
- `w`, `h`: resolucion objetivo del grid.
- `hwaccel`, `hwgpu`: aceleracion de ffmpeg para grid, si tu equipo la soporta.
- `vlcFfmpeg`: usa ffmpeg para grid VLC en vez de VLC mosaic.
- `textOnly`: reduce imagenes en la interfaz.
- `accentColor`: color de acento visual.
- `clientId`: Client ID de Twitch para API.
- `token`: token OAuth de Twitch. Se migra automaticamente a almacenamiento seguro cuando el servidor arranca.
- `miningEnabled`: habilita funciones de puntos/drops si hay token valido.

## Configurar Twitch API

StreamHub funciona para abrir streams publicos sin token, pero algunas funciones necesitan credenciales:

- Estado en vivo enriquecido.
- Canales seguidos.
- Categorias y recomendaciones.
- Drops, puntos de canal y PubSub.

Pasos:

1. Abre StreamHub.
2. Entra en Settings.
3. Pega tu OAuth token en el campo de token.
4. Pega tu Client ID en el campo correspondiente.
5. Guarda la configuracion.

Puedes obtener token y Client ID desde una herramienta de generacion de tokens de Twitch. Usa scopes acordes a las funciones que quieres usar, especialmente lectura de usuario, seguidos, drops y puntos de canal.

El token no se conserva en texto plano dentro del frontend. El backend intenta guardarlo con el llavero del sistema y, si no esta disponible, usa un archivo cifrado local en `runtime app/`.

## Uso Basico

1. Escribe un canal o URL de Twitch en el buscador.
2. Pulsa Enter o el boton de agregar.
3. Usa los controles del canal para abrir, pausar, enviar a grid, alternar audio-only o quitarlo.
4. Usa `Launch` para iniciar la cola segun el reproductor configurado.
5. Usa `Grid` para abrir varios canales en una sola composicion.
6. Usa Settings para cambiar calidad, reproductor, rutas y tema visual.

## Modos De Reproduccion

- `mpv`: recomendado para bajo consumo y ventanas individuales.
- `VLC`: util si prefieres VLC o necesitas su mosaic.
- `GridPlayer`: opcion externa para grid, si lo tienes instalado.
- `audio-only`: abre solo audio de un canal para ahorrar recursos.
- `text-only`: reduce previews e imagenes en la UI.

## Puntos, Drops Y Mining

Las funciones de puntos/drops requieren token OAuth valido y Client ID.

Desde la app puedes:

- Ver estado de conexion.
- Iniciar o detener mining.
- Seguir drops disponibles.
- Ver logs internos.
- Marcar canales para watch/mining.

Si algo falla, revisa el estado de credenciales en Settings y los logs en `logs app/` y `logs streams/`.

## Archivos Locales

Estos archivos y carpetas son generados localmente y no deben subirse a GitHub:

- `.venv/`
- `config.json`
- `config.json.bak`
- `logs app/`
- `logs streams/`
- `runtime app/`
- `__pycache__/`
- backups `*.bak`
- logs `*.log`

`config.example.json` si se versiona porque no contiene credenciales reales.

## Estructura Del Proyecto

- `StreamHub.html`: interfaz principal.
- `server.py`: servidor local, API, reproductores, grid y configuracion.
- `launcher.py`: launcher local alternativo en puerto 8081.
- `secure_token.py`: almacenamiento seguro/migracion de token.
- `pubsub.py`: conexion PubSub para puntos de canal.
- `twitch_gql.py`: cliente GraphQL usado por drops/puntos.
- `install.bat` / `install.sh`: instaladores.
- `iniciar.bat` / `iniciar.sh`: scripts de arranque.
- `logo.png`: logo usado por la interfaz.

## Troubleshooting

### El stream no abre

- Verifica que Streamlink este instalado dentro de `.venv`.
- Revisa que `mpv`, `vlc` o `gridplayer` existan en PATH o que la ruta en Settings sea correcta.
- Prueba otra calidad, por ejemplo `480p` o `best`.

### El grid no abre

- Confirma que `ffmpeg` este instalado.
- Baja la resolucion `w`/`h` o la calidad de streams.
- Desactiva `hwaccel` si tu GPU no soporta el encoder elegido.
- Revisa `logs streams/grid.log`.

### No aparecen seguidos, drops o puntos

- Configura `token` y `clientId`.
- Comprueba que el token no este expirado.
- Guarda Settings y recarga la pagina.
- Revisa `logs app/`.

### El token desaparece al recargar

StreamHub no muestra el token despues de guardarlo por seguridad. Si Settings indica `OK token`, el token sigue guardado en el backend.

## Seguridad

No subas `config.json`, logs, caches ni archivos de runtime. Pueden contener rutas personales, tokens o datos de sesion. El `.gitignore` del proyecto ya los excluye.

## Licencia

Proyecto personal. Define una licencia antes de aceptar contribuciones externas o distribuir releases publicos.
