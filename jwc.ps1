param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $Args
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $root

try {
    $profile = 'debug'
    $actualArgs = @()
    foreach ($a in $Args) {
        if ($a -eq '--release' -or $a -eq '-r') {
            $profile = 'release'
            continue
        }
        if ($a -eq '--debug') {
            $profile = 'debug'
            continue
        }
        $actualArgs += $a
    }

    $exe = Join-Path $root ("target\{0}\jwc.exe" -f $profile)

    $needBuild = -not (Test-Path $exe)
    if (-not $needBuild) {
        $exeTime = (Get-Item $exe).LastWriteTimeUtc
        $inputs = @(
            (Join-Path $root 'Cargo.toml'),
            (Join-Path $root 'Cargo.lock')
        )

        $rsFiles = Get-ChildItem -Path (Join-Path $root 'src') -Recurse -Filter *.rs -File -ErrorAction SilentlyContinue
        foreach ($f in $rsFiles) { $inputs += $f.FullName }

        foreach ($p in $inputs) {
            if (Test-Path $p) {
                $t = (Get-Item $p).LastWriteTimeUtc
                if ($t -gt $exeTime) { $needBuild = $true; break }
            }
        }
    }

    if ($needBuild) {
        if ($profile -eq 'release') {
            Write-Host 'Building jwc (release)...'
            cargo build --release
        }
        else {
            Write-Host 'Building jwc (debug)...'
            cargo build
        }
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }

    & $exe @actualArgs
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
