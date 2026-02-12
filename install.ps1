param(
    [switch] $Release = $true,
    [switch] $Debug,
    [string] $ExePath
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $root

try {
    $profile = 'release'
    if ($Debug) { $profile = 'debug' }

    $exeSrc = $null
    if ($ExePath) {
        $exeSrc = $ExePath
    }
    elseif (Test-Path (Join-Path $root ("target\{0}\jwc.exe" -f $profile))) {
        $exeSrc = (Join-Path $root ("target\{0}\jwc.exe" -f $profile))
    }
    elseif (Test-Path (Join-Path $root 'jwc.exe')) {
        $exeSrc = (Join-Path $root 'jwc.exe')
    }
    elseif (Test-Path (Join-Path $root 'bin\jwc.exe')) {
        $exeSrc = (Join-Path $root 'bin\jwc.exe')
    }
    else {
        $hasCargo = $false
        try { Get-Command cargo -ErrorAction Stop | Out-Null; $hasCargo = $true } catch { $hasCargo = $false }

        if (-not $hasCargo) {
            throw "Rust/cargo not found and no prebuilt jwc.exe found. Provide -ExePath <path-to-jwc.exe> or place a prebuilt binary at .\jwc.exe or .\bin\jwc.exe."
        }

        Write-Host "Building jwc ($profile)..."
        if ($profile -eq 'release') {
            cargo build --release
        } else {
            cargo build
        }
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

        $exeSrc = Join-Path $root ("target\{0}\jwc.exe" -f $profile)
        if (-not (Test-Path $exeSrc)) {
            throw "Build succeeded but jwc.exe not found at $exeSrc"
        }
    }

    if (-not (Test-Path $exeSrc)) {
        throw "jwc.exe not found at $exeSrc"
    }

    Write-Host "Using binary source: $exeSrc"

    $installDir = Join-Path $env:LOCALAPPDATA 'jwc\bin'
    New-Item -ItemType Directory -Force -Path $installDir | Out-Null

    $exeDst = Join-Path $installDir 'jwc.exe'
    Copy-Item -Force $exeSrc $exeDst

    # Add installDir to USER PATH if missing
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($userPath)) { $userPath = '' }

    $pathParts = $userPath -split ';' | Where-Object { $_ -and $_.Trim() -ne '' }
    $already = $false
    foreach ($p in $pathParts) {
        if ($p.TrimEnd('\\') -ieq $installDir.TrimEnd('\\')) { $already = $true; break }
    }

    if (-not $already) {
        $newPath = if ($userPath.Trim().Length -eq 0) { $installDir } else { "$userPath;$installDir" }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added to user PATH: $installDir"
        Write-Host 'Restart your terminal (or sign out/in) to apply everywhere.'
    } else {
        Write-Host "Already on user PATH: $installDir"
    }

    # Also add to current session PATH so `jwc` can be used immediately.
    $procParts = ($env:Path -split ';') | Where-Object { $_ -and $_.Trim() -ne '' }
    $procHas = $false
    foreach ($p in $procParts) {
        if ($p.TrimEnd('\\') -ieq $installDir.TrimEnd('\\')) { $procHas = $true; break }
    }
    if (-not $procHas) {
        $env:Path = "$env:Path;$installDir"
    }

    Write-Host "Installed: $exeDst"
    Write-Host 'Try: jwc --help'
}
finally {
    Pop-Location
}
