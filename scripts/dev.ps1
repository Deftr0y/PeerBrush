param([string]$Action = 'run')
$ErrorActionPreference = 'Stop'
$peerBrushRoot = Split-Path -Parent $PSScriptRoot
Push-Location $peerBrushRoot
try {
    if ($Action -eq 'fmt') {
        cargo fmt --all
    } else {
        if ($Action -eq 'test') {
            & python (Join-Path $PSScriptRoot 'jxl_build.py') -- build --locked --bin peerbrush
            if ($LASTEXITCODE -ne 0) { throw "Worker build failed ($LASTEXITCODE)" }
        }
        $peerBrushArgs = switch ($Action) {
            'run' { @('run', '--locked', '--', '--state-dir', (Join-Path $peerBrushRoot '.runtime')) }
            'check' { @('check', '--locked') }
            'test' { @('test', '--locked') }
            'build' { @('build', '--locked', '--release') }
            default { throw 'Use run, check, test, build, or fmt.' }
        }
        & python (Join-Path $PSScriptRoot 'jxl_build.py') -- @peerBrushArgs
    }
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed ($LASTEXITCODE)" }
} finally { Pop-Location }
