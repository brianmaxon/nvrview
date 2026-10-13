<#
.SYNOPSIS
  Prepares a Windows computer to build and test nvrview.

.DESCRIPTION
  Installs (with winget) Git, the Rust toolchain (rustup, MSVC target), the
  Visual Studio C++ build tools, the .NET 10 SDK, LLVM (clang), Python, and optionally ffmpeg
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
    .\setup-windows.ps1 [-RepoDir <folder>] [-SkipTests] [-Optional]
                        [-EnableSsh [-SshSource winget|feature]
                         -AuthorizedKey "ssh-ed25519 AAAA... name"]
  Only the SSH server and a key (nothing else installed):
    .\setup-windows.ps1 -SshOnly -AuthorizedKey "ssh-ed25519 AAAA... name"

.PARAMETER RepoDir
  Where to clone the repository (default: $HOME\nvrview\repo).
.PARAMETER SkipTests
  Do not run cargo test at the end.
.PARAMETER Optional
  Also install ffmpeg (some tests compare against ffprobe) and gitleaks (the
  secret scan scripts/check.sh runs).
.PARAMETER EnableSsh
  Install and start the OpenSSH server, key-only, reachable from the local
  subnet only.
.PARAMETER SshOnly
  Only turn on the OpenSSH server and add -AuthorizedKey, as -EnableSsh does:
  no toolchain, no repository, no tests. For adding or replacing a key.
.PARAMETER SshSource
  Where the OpenSSH server comes from when none is installed: 'winget' (the
  default: Microsoft's Win32-OpenSSH package, Microsoft.OpenSSH.Preview) or
  'feature' (the Windows optional feature from Windows Update, which can hang
  when Windows Update cannot deliver it).
.PARAMETER AuthorizedKey
  A public key (one line) allowed to log in over SSH. For an administrator
  account it goes to C:\ProgramData\ssh\administrators_authorized_keys, as
  Windows requires; otherwise to the user's .ssh\authorized_keys.
#>
[CmdletBinding()]
param(
  [string]$RepoDir = (Join-Path $HOME 'nvrview\repo'),
  [switch]$SkipTests,
  [switch]$Optional,
  [switch]$EnableSsh,
  [switch]$SshOnly,
  [ValidateSet('winget', 'feature')]
  [string]$SshSource = 'winget',
  [string]$AuthorizedKey = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($SshOnly) {
  $EnableSsh = $true
  $SkipTests = $true
}

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
  # Every package here is in the community 'winget' source. Naming it keeps a
  # broken msstore source (an outdated App Installer fails its certificate
  # check) from stopping the install.
  $wingetArgs = @('install', '--id', $Id, '--exact', '--silent', '--source', 'winget',
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

if (-not $SshOnly) {
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

  Step '.NET SDK 10 (C# bindings and smoke test in apps\windows)'
  Install-WingetPackage 'Microsoft.DotNet.SDK.10'

  Step 'LLVM (clang builds the C layout check against the MSVC ABI, as CI does)'
  Install-WingetPackage 'LLVM.LLVM'

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

  Step 'sccache (shared compile cache for the check workers)'
  if (Get-Command sccache -ErrorAction SilentlyContinue) {
    Write-Host 'sccache already installed'
  } else {
    try { Install-WingetPackage 'Mozilla.sccache' } catch { Write-Host "winget could not install sccache: $_" }
    Sync-SessionPath
    if (-not (Get-Command sccache -ErrorAction SilentlyContinue)) {
      cargo install sccache --locked
    }
  }

  Step 'cargo-nextest (per-test time limits, .config\nextest.toml)'
  if (Get-Command cargo-nextest -ErrorAction SilentlyContinue) {
    Write-Host 'cargo-nextest already installed'
  } else {
    cargo install cargo-nextest --locked
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
}

if ($EnableSsh) {
  Step 'OpenSSH server (key-only, local subnet only)'
  if (Get-Service -Name sshd -ErrorAction SilentlyContinue) {
    Write-Host 'an sshd service already exists; not installing another OpenSSH server'
  } elseif ($SshSource -eq 'winget') {
    Install-WingetPackage 'Microsoft.OpenSSH.Preview'
  } else {
    $cap = Get-WindowsCapability -Online -Name 'OpenSSH.Server*' | Select-Object -First 1
    Write-Host 'installing the OpenSSH server from Windows Update: this often takes 2-10 minutes'
    Write-Host 'with no progress shown (if it stays NotPresent, stop and use -SshSource winget)'
    try {
      Add-WindowsCapability -Online -Name $cap.Name | Out-Null
    } catch {
      Write-Host "Add-WindowsCapability failed ($($_.Exception.Message)); trying DISM"
      dism /Online /Add-Capability /CapabilityName:$($cap.Name) /NoRestart
      if ($LASTEXITCODE -ne 0) { throw 'Could not install the OpenSSH server; try -SshSource winget' }
    }
  }
  if (-not (Get-Service -Name sshd -ErrorAction SilentlyContinue)) {
    throw 'No sshd service after installing the OpenSSH server.'
  }
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
  # The Windows feature names its rule OpenSSH-Server-In-TCP; the winget
  # package may use another name. Disable every OpenSSH inbound rule but ours.
  Get-NetFirewallRule -Direction Inbound -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -ne 'nvrview-ssh-local' -and
      ($_.Name -like 'OpenSSH*' -or $_.DisplayName -like '*OpenSSH*' -or $_.DisplayName -like '*sshd*') } |
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
if (-not $SshOnly) { Write-Host "Repository: $RepoDir" }
Write-Host 'Open a new terminal so PATH changes take effect everywhere.'
if ($EnableSsh) {
  $ips = Get-NetIPAddress -AddressFamily IPv4 |
    Where-Object { $_.PrefixOrigin -in 'Dhcp', 'Manual' -and $_.IPAddress -notlike '169.254.*' } |
    Select-Object -ExpandProperty IPAddress
  Write-Host "SSH: ssh $env:USERNAME@<one of: $($ips -join ', ')>"
}
