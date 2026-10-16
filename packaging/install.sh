#!/bin/sh
# Instala mishapi para el usuario actual, sin sudo: binarios en ~/.local/bin y acceso en el menú de aplicaciones.
#   ./install.sh               instala (desde el paquete descargado, o compila si se corre desde el repo)
#   ./install.sh --uninstall   desinstala (tus datos en ~/.local/share/mishapi no se tocan)
# PREFIX cambia el destino (por defecto ~/.local).
set -eu

prefix=${PREFIX:-$HOME/.local}
apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
here=$(cd "$(dirname "$0")" && pwd)

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$prefix/bin/mishapi" "$prefix/bin/mishapi-cli" "$apps/mishapi.desktop"
    echo "mishapi desinstalado."
    exit 0
fi

if [ -x "$here/mishapi" ]; then
    bin=$here
else
    root=$(dirname "$here")
    echo "Compilando (cargo build --release)..."
    (cd "$root" && cargo build --release)
    bin=$root/target/release
fi

mkdir -p "$prefix/bin" "$apps"
install -m 755 "$bin/mishapi" "$bin/mishapi-cli" "$prefix/bin/"
cat > "$apps/mishapi.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=mishapi
Comment=Cliente de APIs REST agentic first
Exec=$prefix/bin/mishapi
Icon=applications-internet
Terminal=false
Categories=Development;Network;
DESKTOP

echo "Instalado en $prefix/bin (app: mishapi · consola: mishapi-cli)."
case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "Agrega $prefix/bin a tu PATH para usar mishapi-cli desde la terminal." ;;
esac
