param(
    [string]$WasmBindgen = 'wasm-bindgen',
    [string]$WasmBinary = '',
    [switch]$SkipNpmInstall
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Push-Location $root
try {
    if ((& $WasmBindgen --version) -ne 'wasm-bindgen 0.2.130') {
        throw 'Expected wasm-bindgen 0.2.130.'
    }
    if (-not $WasmBinary) {
        cargo build --release --locked --target wasm32-unknown-unknown -p nexdesk-browser-core
        if ($LASTEXITCODE) { throw 'Rust WebAssembly build failed.' }
        $buildRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
        $WasmBinary = Join-Path $buildRoot 'wasm32-unknown-unknown/release/nexdesk_browser_core.wasm'
    }
    & $WasmBindgen --target web --out-dir apps/web/public/core --out-name nexdesk_core $WasmBinary
    if ($LASTEXITCODE) { throw 'WebAssembly bindings generation failed.' }
    Set-Location (Join-Path $root 'apps/web')
    if (-not $SkipNpmInstall) {
        npm.cmd ci --no-audit --no-fund
        if ($LASTEXITCODE) { throw 'Browser dependency installation failed.' }
    }
    npm.cmd run build
    if ($LASTEXITCODE) { throw 'Browser application build failed.' }
} finally {
    Pop-Location
}
