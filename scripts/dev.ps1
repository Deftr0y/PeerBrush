param([string]$Action = 'run')
$ErrorActionPreference = 'Stop'
$peerBrushRoot = Split-Path -Parent $PSScriptRoot
$peerBrushRust = Join-Path $peerBrushRoot '.dev-tools\rust'
if (Test-Path -LiteralPath (Join-Path $peerBrushRust 'bin\cargo.exe')) {
    $env:PATH = "$peerBrushRoot\.dev-tools\binutils\mingw64\bin;$peerBrushRust\bin;$peerBrushRust\lib\rustlib\x86_64-pc-windows-gnu\bin;$peerBrushRust\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained;$env:PATH"
    $env:CARGO_HOME = Join-Path $peerBrushRoot '.dev-tools\cargo-home'
    $env:RUSTFLAGS = '-C linker=rust-lld -C linker-flavor=ld.lld -C link-self-contained=yes'
}
Push-Location $peerBrushRoot
try {
    switch ($Action) {
        'run' { cargo run -- --state-dir (Join-Path $peerBrushRoot '.runtime') }
        'check' { cargo check }
        'test' { cargo test }
        'build' { cargo build --release }
        'fmt' { cargo fmt --all }
        default { throw 'Use run, check, test, build, or fmt.' }
    }
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed ($LASTEXITCODE)" }
} finally { Pop-Location }
