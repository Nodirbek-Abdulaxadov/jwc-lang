$ErrorActionPreference = 'Stop'

$installDir = Join-Path $env:LOCALAPPDATA 'jwc\bin'
$exeDst = Join-Path $installDir 'jwc.exe'

if (Test-Path $exeDst) {
    Remove-Item -Force $exeDst
    Write-Host "Removed: $exeDst"
}

# Remove installDir from USER PATH (best-effort)
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not [string]::IsNullOrWhiteSpace($userPath)) {
    $parts = $userPath -split ';' | Where-Object { $_ -and $_.Trim() -ne '' }
    $filtered = @()
    foreach ($p in $parts) {
        if ($p.TrimEnd('\\') -ine $installDir.TrimEnd('\\')) {
            $filtered += $p
        }
    }
    $newPath = ($filtered -join ';')
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host "Removed from user PATH (if present): $installDir"
    Write-Host 'Restart your terminal (or sign out/in) to apply.'
}

if (Test-Path $installDir) {
    # remove directory only if empty
    $items = Get-ChildItem -Path $installDir -Force -ErrorAction SilentlyContinue
    if (-not $items) {
        Remove-Item -Force $installDir
    }
}
