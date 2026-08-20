#requires -version 5.1
<#
    jwc — one-liner installer (Windows PowerShell).

    iwr -useb https://raw.githubusercontent.com/just-web-code/jwc-lang/main/install.ps1 | iex

    Override targets via env:
      $env:JWC_VERSION          install a specific release tag (e.g. v0.2.0)
      $env:JWC_INSTALL_DIR      destination folder (default: %LOCALAPPDATA%\jwc\bin)
      $env:JWC_DOWNLOAD_BASE    download from a mirror (e.g. the project's MinIO)
                                instead of GitHub Releases. Asset name expected
                                there is "jwc-$VERSION-x86_64-windows.zip".
#>

$ErrorActionPreference = 'Stop'

$Repo = 'just-web-code/jwc-lang'
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

    # Resolve without touching api.github.com. The API caps unauthenticated
    # clients at 60 requests/hour per IP, and a shared NAT — mobile tethering,
    # an office, a VPN exit — burns that on everyone's behalf, so the install
    # dies with a 403 that has nothing to do with this machine.
    # `/releases/latest` is a plain redirect to the tag page and is not part
    # of that budget.
    $Version = $null
    try {
        $resp = Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/$Repo/releases/latest"
        # Windows PowerShell 5.1 exposes ResponseUri; PowerShell 7+ moved it
        # to RequestMessage.RequestUri. Try both rather than pinning a host.
        $final = $null
        if ($resp.BaseResponse.PSObject.Properties.Name -contains 'ResponseUri') {
            $final = $resp.BaseResponse.ResponseUri.AbsoluteUri
        } elseif ($resp.BaseResponse.RequestMessage) {
            $final = $resp.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
        }
        if ($final -match '/releases/tag/(.+)$') { $Version = $Matches[1] }
    } catch { }

    if (-not $Version) {
        try {
            $headers = @{}
            if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $($env:GITHUB_TOKEN)" }
            $Version = (Invoke-RestMethod -UseBasicParsing -Headers $headers `
                -Uri "https://api.github.com/repos/$Repo/releases/latest").tag_name
        } catch { }
    }

    if (-not $Version) {
        Write-Error @"
Failed to resolve the latest release tag for $Repo.

On a tethered, shared or corporate network the usual cause is GitHub's
unauthenticated API limit - 60 requests per hour per IP address, shared with
everyone behind the same NAT.

Skip the lookup by pinning a version (current tags:
https://github.com/$Repo/releases):

  `$env:JWC_VERSION = 'vX.Y.Z'
  iex "& { `$(irm https://raw.githubusercontent.com/$Repo/main/install.ps1) }"

Or authenticate, which raises the limit to 5000/hour:

  `$env:GITHUB_TOKEN = '<a personal access token>'
"@
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

    # Verify the sha256 checksum when the release publishes one (releases
    # after v0.4.1 do). Older releases lack the .sha256 asset — warn and go on.
    try {
        $sumFile = Join-Path $tmp "$asset.sha256"
        Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256" -OutFile $sumFile
        $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0].ToLower()
        $actual   = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
        if ($expected -ne $actual) {
            Write-Error "Checksum mismatch for ${asset}: expected $expected, got $actual. Aborting."
        }
        Write-Host 'sha256 checksum OK.'
    } catch [System.Net.WebException], [Microsoft.PowerShell.Commands.HttpResponseException] {
        Write-Warning "$asset.sha256 not published for $Version — skipping verification."
    }

    Expand-Archive -Path $archive -DestinationPath $tmp -Force

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Force (Join-Path $tmp 'jwc.exe') (Join-Path $InstallDir 'jwc.exe')
    Write-Host "Installed: $InstallDir\jwc.exe"

    # `jwc-lsp` is not built at the moment: it was written against the
    # pre-1.0 parser, which v0.25.0 removed, and it returns rewritten in
    # v0.27.0. Older release archives still carry it, so install it when
    # the archive has one rather than failing on its absence.
    $lsp = Join-Path $tmp 'jwc-lsp.exe'
    if (Test-Path $lsp) {
        Copy-Item -Force $lsp (Join-Path $InstallDir 'jwc-lsp.exe')
        Write-Host "Installed: $InstallDir\jwc-lsp.exe"
    }

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
