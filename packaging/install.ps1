# Instala mishapi para el usuario actual, sin permisos de administrador:
# binarios en %LOCALAPPDATA%\Programs\mishapi, en el PATH del usuario y con acceso en el menú Inicio.
#   .\install.ps1              instala (desde el paquete descargado, o compila si se corre desde el repo)
#   .\install.ps1 -Uninstall   desinstala (tus datos en %APPDATA%\mishapi no se tocan)
param([switch]$Uninstall)
$ErrorActionPreference = 'Stop'

$dest = Join-Path $env:LOCALAPPDATA 'Programs\mishapi'
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'mishapi.lnk'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$paths = @($userPath -split ';' | Where-Object { $_ })

if ($Uninstall) {
    Remove-Item -Recurse -Force $dest, $shortcut -ErrorAction SilentlyContinue
    [Environment]::SetEnvironmentVariable('Path', (($paths | Where-Object { $_ -ne $dest }) -join ';'), 'User')
    Write-Host 'mishapi desinstalado.'
    return
}

$bin = $PSScriptRoot
if (-not (Test-Path (Join-Path $bin 'mishapi.exe'))) {
    $root = Split-Path $PSScriptRoot
    Write-Host 'Compilando (cargo build --release)...'
    cargo build --release --manifest-path (Join-Path $root 'Cargo.toml')
    if ($LASTEXITCODE -ne 0) { throw 'La compilación falló.' }
    $bin = Join-Path $root 'target\release'
}

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $bin 'mishapi.exe'), (Join-Path $bin 'mishapi-cli.exe') $dest -Force
# Queda junto a la app: para desinstalar sin el paquete, corre install.ps1 -Uninstall desde la carpeta de instalación.
if ($PSCommandPath -ne (Join-Path $dest 'install.ps1')) { Copy-Item $PSCommandPath $dest -Force }

if ($paths -notcontains $dest) {
    [Environment]::SetEnvironmentVariable('Path', (($paths + $dest) -join ';'), 'User')
    Write-Host 'Se agregó al PATH del usuario (abre una terminal nueva para usar mishapi-cli).'
}

$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut($shortcut)
$lnk.TargetPath = Join-Path $dest 'mishapi.exe'
$lnk.WorkingDirectory = $dest
$lnk.Save()

Write-Host "Instalado en $dest (app: mishapi · consola: mishapi-cli). Búscalo en el menú Inicio."
