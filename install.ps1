#requires -version 5.1
<#
    jwc — one-liner installer (Windows PowerShell).

    iwr -useb https://raw.githubusercontent.com/Nodirbek-Abdulaxadov/jwc-lang/main/install.ps1 | iex

    Override targets via env:
      $env:JWC_VERSION          install a specific release tag (e.g. v0.2.0)
      $env:JWC_INSTALL_DIR      destination folder (default: %LOCALAPPDATA%\jwc\bin)
      $env:JWC_DOWNLOAD_BASE    download from a mirror (e.g. the project's MinIO)
                                instead of GitHub Releases. Asset name expected
                                there is "jwc-$VERSION-x86_64-windows.zip".
#>

$ErrorActionPreference = 'Stop'

$Repo = 'Nodirbek-Abdulaxadov/jwc-lang'
$InstallDir = if ($env:JWC_INSTALL_DIR) {
    $env:JWC_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'jwc\bin'
}
$DownloadBase = $env:JWC_DOWNLOAD_BASE

# Sanity check architecture — prebuilt JWC binaries ship x86_64 only today.
$arch = (Get-CimInstance Win32_Processor | Select-Object -First 1).Architecture
if ($arch -ne 9) {
    Write-Error "Only x86_64 Windows is supported (CIM architecture=$arch)."
}
$short = 'x86_64-windows'
$ext   = 'zip'

if ($env:JWC_VERSION) {
    $Version = $env:JWC_VERSION
} else {
    Write-Host "Resolving latest release tag for $Repo..."
    $Version = (Invoke-RestMethod -UseBasicParsing `
        -Uri "https://api.github.com/repos/$Repo/releases/latest").tag_name
    if (-not $Version) {
        Write-Error "Failed to resolve latest version. Set `$env:JWC_VERSION to pin one."
    }
}

$asset = "jwc-$Version-$short.$ext"
$url = if ($DownloadBase) {
    "$($DownloadBase.TrimEnd('/'))/$asset"
} else {
    "https://github.com/$Repo/releases/download/$Version/$asset"
}

Write-Host "Downloading $url"
$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "jwc-install-$([guid]::NewGuid().ToString('N'))") -Force
try {
    $archive = Join-Path $tmp $asset
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $archive
    Expand-Archive -Path $archive -DestinationPath $tmp -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Force (Join-Path $tmp 'jwc.exe')     (Join-Path $InstallDir 'jwc.exe')
    Copy-Item -Force (Join-Path $tmp 'jwc-lsp.exe') (Join-Path $InstallDir 'jwc-lsp.exe')

    Write-Host "Installed: $InstallDir\jwc.exe"
    Write-Host "Installed: $InstallDir\jwc-lsp.exe"

    # User-scope PATH update — survives shell restarts. Does not require admin.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($userPath)) { $userPath = '' }
    $already = ($userPath -split ';' | Where-Object {
        $_ -and $_.TrimEnd('\\') -ieq $InstallDir.TrimEnd('\\')
    }).Count -gt 0

    if (-not $already) {
        $newPath = if ($userPath.Trim().Length -eq 0) { $InstallDir } else { "$userPath;$InstallDir" }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added to user PATH: $InstallDir"
        Write-Host 'Open a fresh terminal (or sign out/in) for the new PATH to take effect.'
    } else {
        Write-Host "Already on user PATH: $InstallDir"
    }

    # Make the binary usable in the current session too.
    if (-not (($env:Path -split ';') | Where-Object {
        $_ -and $_.TrimEnd('\\') -ieq $InstallDir.TrimEnd('\\')
    })) {
        $env:Path = "$env:Path;$InstallDir"
    }

    Write-Host ''
    Write-Host 'Try:  jwc --help'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
