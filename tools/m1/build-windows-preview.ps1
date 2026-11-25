param(
    [Parameter(Mandatory)]
    [string]$FlutterRoot,
    [ValidateSet('lib/main.dart', 'test_support/native_render_probe.dart')]
    [string]$DartTarget = 'lib/main.dart',
    [ValidatePattern('^[a-zA-Z0-9-]+$')]
    [string]$DesktopBundle,
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',
    [ValidateLength(0, 512)]
    [string]$DefaultDirectory,
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string]$SigningThumbprint,
    [ValidatePattern('^https://')]
    [string]$TimestampUrl,
    [switch]$SkipCargo,
    [switch]$SkipPub
)

$ErrorActionPreference = 'Stop'
if ($SigningThumbprint -and $Configuration -ne 'Release') {
    throw 'Code signing requires a Release build.'
}
function Check-NativeExit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed ($LASTEXITCODE)." }
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$sdkPath = (Resolve-Path -LiteralPath $FlutterRoot).Path
$flutter = Join-Path $sdkPath 'bin\flutter.bat'
$desktop = Join-Path $root 'apps\desktop'
$runner = Join-Path $desktop 'windows\runner'
$profile = $Configuration.ToLowerInvariant()
$bundleName = if ($DesktopBundle) { $DesktopBundle } elseif ($Configuration -eq 'Release') { 'nexdesk-windows-x64-release' } elseif ($DartTarget -eq 'lib/main.dart') { 'm1-windows-x64' } else { 'm1-render-probe-x64' }
$preview = Join-Path $desktop "build\$bundleName"
$assets = Join-Path $desktop "build\assemble-$bundleName-$profile"
$data = Join-Path $preview 'data'
$objects = Join-Path $preview 'obj'
$previousFlutterRoot = $env:FLUTTER_ROOT

Push-Location -LiteralPath $root
try {
    if (-not $SkipCargo) {
        $cargoArgs = @('build','--locked','-p','nexdesk-ffi','-p','nexdesk-agent')
        if ($Configuration -eq 'Release') { $cargoArgs += '--release' }
        cargo @cargoArgs
        Check-NativeExit 'Rust build'
    }
    $env:FLUTTER_ROOT = $sdkPath
    $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $native = (Resolve-Path -LiteralPath (Join-Path $targetRoot "$profile\nexdesk_ffi.dll")).Path
    $agent = (Resolve-Path -LiteralPath (Join-Path $targetRoot "$profile\nexdesk-agent.exe")).Path
    $desktopDefines = @()
    if ($DartTarget -ne 'lib/main.dart') {
        $desktopDefines += "NEXDESK_CORE_LIBRARY=$native"
    }
    if ($DefaultDirectory) {
        $directoryUri = $null
        if (-not [Uri]::TryCreate($DefaultDirectory, [UriKind]::Absolute, [ref]$directoryUri) -or
            -not $directoryUri.Host -or
            ($directoryUri.Scheme -ne 'https' -and -not ($directoryUri.Scheme -eq 'http' -and $directoryUri.IsLoopback)) -or
            $directoryUri.AbsolutePath -ne '/' -or $directoryUri.UserInfo -or $directoryUri.Query -or $directoryUri.Fragment -or
            $directoryUri.Port -lt 1 -or $directoryUri.Port -gt 65535) {
            throw 'DefaultDirectory must be the root HTTPS origin, or HTTP loopback for a local build.'
        }
        $desktopDefines += "NEXDESK_DEFAULT_DIRECTORY=$($directoryUri.GetLeftPart([UriPartial]::Authority))"
    }
    Set-Location -LiteralPath $desktop
    if (-not $SkipPub) {
        & $flutter --suppress-analytics --no-version-check pub get
        Check-NativeExit 'Flutter dependencies'
    }
    $assembleArgs = @(
        '--suppress-analytics', '--no-version-check', 'assemble', "--output=$assets",
        '-dTargetPlatform=windows-x64', "-dBuildMode=$profile",
        "-dTargetFile=$DartTarget", '-dTreeShakeIcons=false', '-dDartObfuscation=false'
    )
    if ($Configuration -eq 'Debug') { $assembleArgs += '-dTrackWidgetCreation=true' }
    if ($desktopDefines.Count -gt 0) {
        $encodedDefines = $desktopDefines | ForEach-Object {
            [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($_))
        }
        $assembleArgs += "--DartDefines=$($encodedDefines -join ',')"
    }
    $assembleArgs += "${profile}_bundle_windows-x64_assets"
    & $flutter @assembleArgs
    Check-NativeExit 'Flutter asset assembly'

    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $visualStudio = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $visualStudio) { throw 'Install the Visual Studio MSVC x64 compiler and Windows SDK.' }
    Import-Module (Join-Path $visualStudio 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
    Enter-VsDevShell -VsInstallPath $visualStudio -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'

    $engine = Join-Path $desktop 'windows\flutter\ephemeral'
    $wrapper = Join-Path $engine 'cpp_client_wrapper'
    $nativeInclude = Join-Path $root 'crates\nexdesk-ffi\include'
    $engineInclude = Join-Path $wrapper 'include'
    $windowsInclude = Join-Path $desktop 'windows'
    New-Item -ItemType Directory -Path $preview,$data,$objects -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $assets 'flutter_assets') -Destination $data -Recurse -Force
    if ($Configuration -eq 'Release') {
        Copy-Item -LiteralPath (Join-Path $assets 'windows\app.so') -Destination $data -Force
    }
    Copy-Item -LiteralPath (Join-Path $engine 'icudtl.dat') -Destination $data -Force
    Copy-Item -LiteralPath (Join-Path $engine 'flutter_windows.dll') -Destination $preview -Force
    Copy-Item -LiteralPath $native,$agent -Destination $preview -Force

    Set-Location -LiteralPath $runner
    $resource = Join-Path $objects 'runner.res'
    & rc.exe /nologo /fo $resource Runner.rc
    Check-NativeExit 'Runner resources'

    $cppFiles = @('flutter_window.cpp','main.cpp','preview_texture.cpp','utils.cpp','win32_window.cpp') |
        ForEach-Object { Join-Path $runner $_ }
    $cppFiles += Join-Path $desktop 'windows\flutter\generated_plugin_registrant.cc'
    $cppFiles += @('core_implementations.cc','standard_codec.cc','flutter_engine.cc','flutter_view_controller.cc','plugin_registrar.cc') |
        ForEach-Object { Join-Path $wrapper $_ }
    $exe = Join-Path $preview 'nexdesk_desktop.exe'
    $manifest = Join-Path $runner 'runner.exe.manifest'
    $compilerArgs = @(
        '/nologo', '/W4', '/WX', '/wd4100', '/utf-8', '/std:c++17', '/EHsc', '/MD', '/DUNICODE',
        '/D_UNICODE', '/DNOMINMAX', '/D_HAS_EXCEPTIONS=0', "/I$engine", "/I$engineInclude",
        "/I$windowsInclude", "/I$nativeInclude", "/Fo$objects\", "/Fe$exe"
    )
    if ($Configuration -eq 'Release') { $compilerArgs += @('/O2', '/DNDEBUG') }
    $linkerArgs = @(
        '/link', '/SUBSYSTEM:WINDOWS', '/MANIFEST:EMBED', "/MANIFESTINPUT:$manifest",
        (Join-Path $engine 'flutter_windows.dll.lib'), 'user32.lib', 'gdi32.lib',
        'dwmapi.lib', 'shell32.lib', 'ole32.lib', 'advapi32.lib'
    )
    & cl.exe @compilerArgs @cppFiles $resource @linkerArgs
    Check-NativeExit 'Native texture runner'
    if ($SigningThumbprint) {
        Get-Command signtool.exe -ErrorAction Stop | Out-Null
        $signArgs = @('sign', '/sha1', $SigningThumbprint, '/fd', 'SHA256')
        if ($TimestampUrl) { $signArgs += @('/tr', $TimestampUrl, '/td', 'SHA256') }
        foreach ($signedFile in @($exe, (Join-Path $preview 'nexdesk-agent.exe'), (Join-Path $preview 'nexdesk_ffi.dll'))) {
            & signtool.exe @signArgs $signedFile
            Check-NativeExit 'Authenticode signing'
            & signtool.exe verify /pa /v $signedFile
            Check-NativeExit 'Authenticode verification'
        }
    }
    Write-Output $exe
} finally {
    Pop-Location
    $env:FLUTTER_ROOT = $previousFlutterRoot
}
