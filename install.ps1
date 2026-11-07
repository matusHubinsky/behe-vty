# behe-vty installer for Windows.
# Builds the project and installs the binary as three commands:
#   behe-vty, behe-terminal, behe-console
#
# Usage:
#   .\install.ps1              install into %LOCALAPPDATA%\Programs\behe-vty and add to PATH
#   .\install.ps1 -User        same as default (user install, no admin needed)

param(
    [switch]$User
)

$ErrorActionPreference = 'Stop'

$BinName = 'behe-vty'
$Aliases = @('behe-terminal', 'behe-console')

function Log($msg) {
    Write-Host "==> $msg"
}

$Banner = @'
██████╗ ███████╗██╗  ██╗███████╗    ██╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝██║  ██║██╔════╝    ██║   ██║╚══██╔══╝╚██╗ ██╔╝
██████╔╝█████╗  ███████║█████╗█████╗██║   ██║   ██║    ╚████╔╝ 
██╔══██╗██╔══╝  ██╔══██║██╔══╝╚════╝╚██╗ ██╔╝   ██║     ╚██╔╝  
██████╔╝███████╗██║  ██║███████╗     ╚████╔╝    ██║      ██║   
╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝      ╚═══╝     ╚═╝      ╚═╝  
'@
Write-Host $Banner

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error 'cargo not found. Install the Rust toolchain first: https://rustup.rs'
    exit 1
}

Log 'Building release binary...'
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Error 'cargo build failed'
    exit 1
}

$Binary = Join-Path 'target' 'release' "$BinName.exe"
if (-not (Test-Path $Binary)) {
    Write-Error "build finished but $Binary was not found"
    exit 1
}

$DestDir = Join-Path $env:LOCALAPPDATA 'Programs' 'behe-vty'
Log "Installing to $DestDir ..."
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null

Copy-Item $Binary (Join-Path $DestDir "$BinName.exe") -Force

Log "Creating command aliases: $($Aliases -join ', ')"
foreach ($alias in $Aliases) {
    Copy-Item $Binary (Join-Path $DestDir "$alias.exe") -Force
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$DestDir*") {
    Log "Adding $DestDir to user PATH..."
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$DestDir", 'User')
    $env:Path = "$env:Path;$DestDir"
} else {
    $env:Path = "$env:Path;$DestDir"
}

Log 'Done. Available commands: behe-vty, behe-terminal, behe-console'
Log 'Note: open a new terminal for the PATH change to take effect.'

