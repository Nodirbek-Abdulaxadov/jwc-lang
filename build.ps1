param(
    [switch] $Release,
    [switch] $Debug
)

$ErrorActionPreference = 'Stop'

if ($Release -and $Debug) {
    throw "Choose only one profile: -Release or -Debug"
}

$profile = 'debug'
if ($Release) { $profile = 'release' }

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $root

try {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) {
        throw "cargo not found. Install Rust toolchain first: https://rustup.rs"
    }

    $os = [System.Environment]::OSVersion.VersionString
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ([string]::IsNullOrWhiteSpace($arch)) {
        $arch = 'unknown'
    }

    Write-Host "Native build target"
    Write-Host "  OS: $os"
    Write-Host "  Arch: $arch"
    Write-Host "  Profile: $profile"

    if ($profile -eq 'release') {
        cargo build --release
    }
    else {
        cargo build
    }

    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    $binPath = Join-Path $root ("target\{0}\jwc.exe" -f $profile)
    if (-not (Test-Path $binPath)) {
        throw "Build completed but binary was not found: $binPath"
    }

    Write-Host "Build OK"
    Write-Host "Binary: $binPath"
}
finally {
    Pop-Location
}
