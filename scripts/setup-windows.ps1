<#
.SYNOPSIS
  Prepares a Windows computer to build and test nvrview.

.DESCRIPTION
  Installs (with winget) Git, the Rust toolchain (rustup, MSVC target), the
  Visual Studio C++ build tools, the .NET SDK, Python, and optionally ffmpeg
  and gitleaks; installs cbindgen; clones the repository; and runs the Rust
  tests to prove the setup works. Every step checks what is already there,
  so running it again is safe.

  With -EnableSsh it also turns on the Windows OpenSSH server for key-only
  logins from the local network, so a Mac on the same network can run builds
  and tests here over SSH (no GitHub self-hosted runner: on a public
  repository that would run code from anyone's pull request on this
  computer).

  Run from an elevated PowerShell (Run as administrator):
    Set-ExecutionPolicy -Scope Process Bypass
    .\setup-windows.ps1 [-RepoDir C:\src\nvrview] [-SkipTests] [-Optional]
                        [-EnableSsh -AuthorizedKey "ssh-ed25519 AAAA... name"]

.PARAMETER RepoDir
  Where to clone the repository (default: $HOME\src\nvrview).
.PARAMETER SkipTests
  Do not run cargo test at the end.
.PARAMETER Optional
  Also install ffmpeg (some tests compare against ffprobe) and gitleaks (the
  secret scan scripts/check.sh runs).
.PARAMETER EnableSsh
  Install and start the OpenSSH server, key-only, reachable from the local
  subnet only.
.PARAMETER AuthorizedKey
  A public key (one line) allowed to log in over SSH. For an administrator
  account it goes to C:\ProgramData\ssh\administrators_authorized_keys, as
  Windows requires; otherwise to the user's .ssh\authorized_keys.
#>
[CmdletBinding()]
param(
  [string]$RepoDir = (Join-Path $HOME 'src\nvrview'),
  [switch]$SkipTests,
  [switch]$Optional,
  [switch]$EnableSsh,
  [string]$AuthorizedKey = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoUrl = 'https://github.com/brianmaxon/nvrview.git'

function Step([string]$Message) { Write-Host "`n== $Message" -ForegroundColor Cyan }

function Test-Admin {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  (New-Object Security.Principal.WindowsPrincipal $id).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Makes tools installed earlier in this run visible without a new shell.
function Sync-SessionPath {
  $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
  $user = [Environment]::GetEnvironmentVariable('Path', 'User')
  $cargo = Join-Path $HOME '.cargo\bin'
  $env:Path = (@($machine, $user, $cargo) | Where-Object { $_ }) -join ';'
}

function Install-WingetPackage([string]$Id, [string[]]$ExtraArgs = @()) {
  $listed = winget list --id $Id --exact --accept-source-agreements 2>$null | Out-String
  if ($listed -match [regex]::Escape($Id)) {
    Write-Host "$Id already installed"
    return
  }
  Write-Host "installing $Id"
  $wingetArgs = @('install', '--id', $Id, '--exact', '--silent',
    '--accept-package-agreements', '--accept-source-agreements') + $ExtraArgs
  & winget @wingetArgs
  # winget returns non-zero for "already installed" on some versions; check again.
  $listed = winget list --id $Id --exact 2>$null | Out-String
  if ($listed -notmatch [regex]::Escape($Id)) { throw "winget could not install $Id" }
}

if (-not (Test-Admin)) {
  throw 'Run this script from an elevated PowerShell (Run as administrator).'
}
if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
  throw 'winget is missing: install "App Installer" from the Microsoft Store first.'
}

Step 'Git'
Install-WingetPackage 'Git.Git'

Step 'Visual Studio C++ build tools (MSVC linker and Windows SDK, needed by Rust)'
Install-WingetPackage 'Microsoft.VisualStudio.2022.BuildTools' @(
  '--override',
  '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')

Step 'Rust (rustup, stable, MSVC)'
Install-WingetPackage 'Rustlang.Rustup'
Sync-SessionPath
rustup default stable-x86_64-pc-windows-msvc
rustup component add rustfmt clippy
rustc --version

Step '.NET SDK (C# bindings and smoke test in apps\windows)'
Install-WingetPackage 'Microsoft.DotNet.SDK.8'

Step 'Python (scripts\check-sanitized.py)'
Install-WingetPackage 'Python.Python.3.12'

if ($Optional) {
  Step 'Optional: ffmpeg and gitleaks'
  Install-WingetPackage 'Gyan.FFmpeg'
  Install-WingetPackage 'Gitleaks.Gitleaks'
}

Sync-SessionPath

Step 'cbindgen (checks the generated C header)'
if (Get-Command cbindgen -ErrorAction SilentlyContinue) {
  Write-Host 'cbindgen already installed'
} else {
  cargo install cbindgen --locked
}

Step "Repository in $RepoDir"
if (Test-Path (Join-Path $RepoDir '.git')) {
  Write-Host 'already cloned; fetching'
  git -C $RepoDir fetch --quiet origin
} else {
  New-Item -ItemType Directory -Force -Path (Split-Path $RepoDir) | Out-Null
  git clone $RepoUrl $RepoDir
}
# Fixtures are binary and compared byte for byte; keep files exactly as committed.
git -C $RepoDir config core.autocrlf false
git -C $RepoDir config core.longpaths true

if ($EnableSsh) {
  Step 'OpenSSH server (key-only, local subnet only)'
  $cap = Get-WindowsCapability -Online -Name 'OpenSSH.Server*' | Select-Object -First 1
  if ($cap.State -ne 'Installed') { Add-WindowsCapability -Online -Name $cap.Name | Out-Null }
  Set-Service -Name sshd -StartupType Automatic
  Start-Service sshd

  $config = 'C:\ProgramData\ssh\sshd_config'
  $text = Get-Content $config -Raw
  foreach ($setting in @('PasswordAuthentication no', 'PubkeyAuthentication yes')) {
    $name = $setting.Split(' ')[0]
    if ($text -match "(?m)^\s*#?\s*$name\s+.*$") {
      $text = $text -replace "(?m)^\s*#?\s*$name\s+.*$", $setting
    } else {
      $text = "$setting`r`n$text"
    }
  }
  Set-Content -Path $config -Value $text -Encoding ascii
  Restart-Service sshd

  # Replace the default rule (any address) with one for the local subnet only.
  Get-NetFirewallRule -Name 'OpenSSH-Server-In-TCP' -ErrorAction SilentlyContinue |
    Disable-NetFirewallRule
  if (-not (Get-NetFirewallRule -Name 'nvrview-ssh-local' -ErrorAction SilentlyContinue)) {
    New-NetFirewallRule -Name 'nvrview-ssh-local' -DisplayName 'OpenSSH (local subnet only)' `
      -Direction Inbound -Protocol TCP -LocalPort 22 -RemoteAddress LocalSubnet `
      -Action Allow | Out-Null
  }

  if ($AuthorizedKey) {
    if ($AuthorizedKey -notmatch '^(ssh-ed25519|ecdsa-sha2-\S+|ssh-rsa) \S+') {
      throw 'AuthorizedKey does not look like a public key line.'
    }
    # Windows reads administrators' keys from one shared file only.
    $file = 'C:\ProgramData\ssh\administrators_authorized_keys'
    if (-not (Test-Path $file)) { New-Item -ItemType File -Path $file | Out-Null }
    if (-not (Select-String -Path $file -SimpleMatch $AuthorizedKey -Quiet)) {
      Add-Content -Path $file -Value $AuthorizedKey -Encoding ascii
    }
    icacls $file /inheritance:r /grant 'Administrators:F' /grant 'SYSTEM:F' | Out-Null
    Write-Host "key added to $file"
  } else {
    Write-Host 'no -AuthorizedKey given: add one before logging in (password logins are off)'
  }
}

if (-not $SkipTests) {
  Step 'Rust tests (proves the toolchain works; first build takes a while)'
  Push-Location $RepoDir
  try {
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw "cargo test failed ($LASTEXITCODE)" }
  } finally {
    Pop-Location
  }
}

Step 'Done'
Write-Host "Repository: $RepoDir"
Write-Host 'Open a new terminal so PATH changes take effect everywhere.'
if ($EnableSsh) {
  $ips = Get-NetIPAddress -AddressFamily IPv4 |
    Where-Object { $_.PrefixOrigin -in 'Dhcp', 'Manual' -and $_.IPAddress -notlike '169.254.*' } |
    Select-Object -ExpandProperty IPAddress
  Write-Host "SSH: ssh $env:USERNAME@<one of: $($ips -join ', ')>"
}
