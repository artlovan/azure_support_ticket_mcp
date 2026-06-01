# install.ps1 — install azure-support-ticket-mcp on Windows (PowerShell).
#
# Usage:
#   irm https://github.com/artlovan/azure_support_ticket_mcp/releases/latest/download/install.ps1 | iex
#
# Parameters (when running the script directly, not via irm|iex):
#   -Version <vX.Y.Z>      install a specific version (default: latest)
#   -Prefix <directory>    override install directory
#                          (default: $env:LOCALAPPDATA\Programs\azure-support-ticket-mcp)
#
# What it does:
#   1. Detects CPU architecture (only x86_64 is supported in v1).
#   2. Downloads the matching binary + .sha256 sidecar from GitHub Releases.
#   3. Verifies the SHA256 checksum.
#   4. Installs the binary into the install directory.
#   5. Adds the install directory to the user's PATH if not already present.

[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [string]$Prefix  = (Join-Path $env:LOCALAPPDATA 'Programs\azure-support-ticket-mcp')
)

$ErrorActionPreference = 'Stop'

# ---- Configuration --------------------------------------------------------

# Repository to install from. Update these when the project moves.
$Owner   = if ($env:AZURE_SUPPORT_TICKET_MCP_OWNER) { $env:AZURE_SUPPORT_TICKET_MCP_OWNER } else { 'artlovan' }
$Repo    = if ($env:AZURE_SUPPORT_TICKET_MCP_REPO)  { $env:AZURE_SUPPORT_TICKET_MCP_REPO  } else { 'azure_support_ticket_mcp' }

$BinName = 'azure-support-ticket-mcp'

# ---- Platform detection ---------------------------------------------------

$archRaw = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($archRaw) {
    'X64'   { $Arch = 'x86_64' }
    default {
        Write-Error "install.ps1: unsupported CPU architecture: $archRaw. Supported: x86_64."
        exit 1
    }
}

$Asset = "$BinName-windows-$Arch.exe"

# ---- Build download URLs --------------------------------------------------

if ($Version -eq 'latest') {
    $BaseUrl = "https://github.com/$Owner/$Repo/releases/latest/download"
} else {
    $BaseUrl = "https://github.com/$Owner/$Repo/releases/download/$Version"
}

$BinUrl = "$BaseUrl/$Asset"
$ShaUrl = "$BaseUrl/$Asset.sha256"

# ---- Download + verify ----------------------------------------------------

$TmpDir = Join-Path $env:TEMP "azure-support-ticket-mcp-install-$([System.Guid]::NewGuid())"
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    Write-Host "Installing $BinName (windows-$Arch, version: $Version)"
    Write-Host "  source:  $BinUrl"
    Write-Host "  target:  $Prefix\$BinName.exe"

    $BinTmp = Join-Path $TmpDir "$BinName.exe"
    $ShaTmp = Join-Path $TmpDir "$BinName.exe.sha256"

    Invoke-WebRequest -Uri $BinUrl -OutFile $BinTmp -UseBasicParsing
    Invoke-WebRequest -Uri $ShaUrl -OutFile $ShaTmp -UseBasicParsing

    $Expected = (Get-Content -Raw -Path $ShaTmp).Trim().Split()[0].ToLowerInvariant()
    $Actual   = (Get-FileHash -Path $BinTmp -Algorithm SHA256).Hash.ToLowerInvariant()

    if ($Expected -ne $Actual) {
        Write-Error @"
install.ps1: checksum mismatch.
  expected: $Expected
  actual:   $Actual
Refusing to install. Please re-run; if this persists, file an issue.
"@
        exit 1
    }

    Write-Host "  sha256:  $Actual  [verified]"

    # ---- Install --------------------------------------------------------

    if (-not (Test-Path $Prefix)) {
        New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
    }

    $Dest = Join-Path $Prefix "$BinName.exe"
    Move-Item -Path $BinTmp -Destination $Dest -Force

    Write-Host ""
    Write-Host "Installed: $Dest"

    # ---- PATH update ----------------------------------------------------

    $UserPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $PathParts = if ($UserPath) { $UserPath.Split(';') } else { @() }

    if ($PathParts -notcontains $Prefix) {
        $NewPath = if ($UserPath) { "$UserPath;$Prefix" } else { $Prefix }
        [Environment]::SetEnvironmentVariable('PATH', $NewPath, 'User')
        Write-Host ""
        Write-Host "NOTE: Added $Prefix to your user PATH."
        Write-Host "      Open a new PowerShell window for the change to take effect."
    }

    Write-Host ""
    Write-Host "Next: open a new shell and run ``$BinName doctor`` to verify the install."
}
finally {
    Remove-Item -Recurse -Force -Path $TmpDir -ErrorAction SilentlyContinue
}
