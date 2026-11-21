<#
.SYNOPSIS
Installs the compiler on Windows.

.DESCRIPTION
The compiler and the programs it builds are Linux x86-64 only (docs/building.md), so this script
sets up Ubuntu 24.04 in WSL 2 if it is not there, installs the compiler in it with
scripts/install.sh (which installs LLVM 19, the Boehm collector, a C toolchain and Rust), and puts
a `viper.cmd` command on the Windows PATH that runs the compiler in WSL, in the folder you are in.

Windows 10 version 2004 or later, or Windows 11, with virtualisation enabled. Installing WSL needs
an administrator PowerShell and may ask for a restart; if it does, restart and run this script again.
Programs that `viper build` writes are Linux executables: run them with `viper run app.vi`, or
inside WSL. Servers listening in WSL are reachable on localhost from Windows.

.PARAMETER Distro
The WSL distribution to use (default Ubuntu-24.04).

.PARAMETER Yes
Do not ask before installing.

.EXAMPLE
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
#>
param(
    [string]$Distro = "Ubuntu-24.04",
    [switch]$Yes
)
$ErrorActionPreference = "Stop"

function Say($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Die($m) { Write-Host "error: $m" -ForegroundColor Red; exit 1 }

if ($env:OS -ne "Windows_NT") { Die "this script is for Windows; on Linux run scripts/install.sh, on macOS scripts/install-macos.sh" }
if ($env:PROCESSOR_ARCHITECTURE -ne "AMD64") { Die "the compiler supports x86-64 only; this machine is $($env:PROCESSOR_ARCHITECTURE)" }

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not ((Test-Path (Join-Path $root "Cargo.toml")) -and (Test-Path (Join-Path $root "scripts\install.sh")))) {
    Die "run this script from a checkout of the repository"
}

Say "This will set up $Distro in WSL 2 if needed, install the compiler in it (system packages, LLVM 19, Rust), and add viper.cmd to your PATH."
if (-not $Yes) {
    $answer = Read-Host "Continue? [y/N]"
    if ($answer -notmatch '^(y|yes)$') { Die "stopped; nothing was changed" }
}

# The names of the installed distributions. wsl.exe writes UTF-16, which shows as text with NUL bytes.
function Get-Distros {
    if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) { return @() }
    $out = & wsl.exe -l -q 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $out) { return @() }
    return @($out | ForEach-Object { ($_ -replace "`0", "").Trim() } | Where-Object { $_ })
}

if ((Get-Distros) -notcontains $Distro) {
    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $admin) { Die "installing WSL needs an administrator PowerShell: open one (right-click, Run as administrator) and run this script again" }
    Say "Installing $Distro in WSL 2 (downloads the distribution)"
    & wsl.exe --install -d $Distro --no-launch
    if ($LASTEXITCODE -ne 0) { Die "wsl --install failed (exit $LASTEXITCODE); check that virtualisation is enabled, and see https://aka.ms/wslinstall" }
    if ((Get-Distros) -notcontains $Distro) {
        Die "WSL is installed, but $Distro is not ready yet. Restart Windows, then run this script again"
    }
}

# Everything below runs in the distribution as root, so no user has to be set up first.
function Invoke-Wsl([string]$script) {
    & wsl.exe -d $Distro -u root -- sh -c $script
    if ($LASTEXITCODE -ne 0) { Die "a step failed in WSL (exit $LASTEXITCODE): $script" }
}

Say "Copying the sources into $Distro"
$wslRoot = (& wsl.exe -d $Distro -u root -- wslpath -a ($root -replace '\\', '/')).Trim()
if (-not $wslRoot) { Die "could not translate $root to a WSL path" }
Invoke-Wsl "rm -rf /opt/viper-src && mkdir -p /opt/viper-src && tar -C '$wslRoot' --exclude=./target --exclude=./.git -cf - . | tar -C /opt/viper-src -xf -"
# A Windows checkout may have CRLF line endings, which the shell scripts and the Makefile cannot run with.
Invoke-Wsl "sed -i 's/\r$//' /opt/viper-src/scripts/*.sh /opt/viper-src/Makefile"

Say "Installing the compiler in $Distro (this takes a few minutes)"
Invoke-Wsl "sh /opt/viper-src/scripts/install.sh --yes --prefix /usr/local"

Say "Adding viper.cmd to your PATH"
$bin = Join-Path $env:LOCALAPPDATA "Viper\bin"
New-Item -ItemType Directory -Force -Path $bin | Out-Null
# wsl.exe starts in the current folder, translated to /mnt/..., and passes the exit status back.
$cmd = "@echo off`r`nwsl.exe -d $Distro -- viper %*`r`nexit /b %ERRORLEVEL%`r`n"
[IO.File]::WriteAllText((Join-Path $bin "viper.cmd"), $cmd, [Text.Encoding]::ASCII)

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $bin) {
    [Environment]::SetEnvironmentVariable("Path", ($userPath.TrimEnd(";") + ";" + $bin), "User")
    Say "Added $bin to your PATH. Open a new terminal to use it."
}

Say "Done. In a new terminal, try: viper --version"
