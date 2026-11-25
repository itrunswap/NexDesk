param([string]$Destination)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Destination) { $Destination = Join-Path $root 'target\m1-deps' }
New-Item -ItemType Directory -Path $Destination -Force | Out-Null
$cache = (Resolve-Path -LiteralPath $Destination).Path
$sdk = Join-Path $cache 'win-x64-release'
$archive = Join-Path $cache 'webrtc-win-x64-release.zip'
$sha256 = '862403fcb248c7d8803487e4c22b8821c6130e89100cb9c69be95ef45ff46146'
$url = 'https://github.com/livekit/rust-sdks/releases/download/webrtc-a545de4/webrtc-win-x64-release.zip'

if (-not (Test-Path -LiteralPath (Join-Path $sdk 'lib\webrtc.lib'))) {
    if (-not (Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest -Uri $url -OutFile $archive -TimeoutSec 180
    }
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    if ($actual.ToLowerInvariant() -ne $sha256) {
        throw 'The WebRTC SDK archive failed its pinned SHA-256 check.'
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($archive, $cache)
}
if (-not (Test-Path -LiteralPath (Join-Path $sdk 'webrtc.ninja'))) {
    throw 'The extracted WebRTC SDK is incomplete.'
}
Write-Output $sdk
