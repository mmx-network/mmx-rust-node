# Builds the MMX Wallet for Windows 10/11 x64 as an NSIS installer in
# artifacts/packages. Run from PowerShell 7 on Windows with the Visual Studio
# C++ build tools, rustup (MSVC toolchain), Node.js and Git for Windows, after
# `npm run build:wasm` (it runs the WebAssembly build in Git for Windows' bash).
#
# The installer installs for the current user (no administrator rights) and
# installs Microsoft WebView2 if it is missing (needs internet access).
#
# Signing (optional): WINDOWS_CERTIFICATE_THUMBPRINT selects a code signing
# certificate in the certificate store; WINDOWS_TIMESTAMP_URL sets the
# timestamp server (default http://timestamp.digicert.com).
#Requires -Version 7.0
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Native([scriptblock] $Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "Command failed (exit $LASTEXITCODE): $Command" }
}
function Write-Utf8([string] $Path, [string] $Text) {
    [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false))
}

if (-not $IsWindows) { throw 'Run this build on Windows (PowerShell 7).' }
$wallet = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$artifacts = Join-Path $wallet 'artifacts'
$targetDir = Join-Path $artifacts 'build-windows'
$packages = Join-Path $artifacts 'packages'
$target = 'x86_64-pc-windows-msvc'
$version = (Get-Content (Join-Path $wallet 'package.json') -Raw | ConvertFrom-Json).version
$product = (Get-Content (Join-Path $wallet 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json).productName

if (-not (Test-Path (Join-Path $wallet 'src/backend/web/pkg/mmx_wallet_wasm.js'))) {
    throw 'The web wallet core is missing (needed to check the shared interface): run npm run build:wasm'
}
New-Item -ItemType Directory -Force -Path $targetDir, $packages | Out-Null
Invoke-Native { rustup target add $target }

$extraConfig = @()
if ($env:WINDOWS_CERTIFICATE_THUMBPRINT) {
    $timestamp = if ($env:WINDOWS_TIMESTAMP_URL) { $env:WINDOWS_TIMESTAMP_URL } else { 'http://timestamp.digicert.com' }
    $signing = @{ bundle = @{ windows = @{
        certificateThumbprint = $env:WINDOWS_CERTIFICATE_THUMBPRINT
        digestAlgorithm = 'sha256'
        timestampUrl = $timestamp
    } } }
    $signingFile = Join-Path $targetDir 'signing.json'
    Write-Utf8 $signingFile ($signing | ConvertTo-Json -Depth 5)
    $extraConfig = @('--config', $signingFile)
}

$saved = @{ CARGO_TARGET_DIR = $env:CARGO_TARGET_DIR; CARGO_INCREMENTAL = $env:CARGO_INCREMENTAL }
Push-Location $wallet
try {
    $env:CARGO_TARGET_DIR = $targetDir
    $env:CARGO_INCREMENTAL = '0'
    Invoke-Native { npm.cmd ci --no-audit --no-fund }
    Invoke-Native { npm.cmd run tauri build -- --target $target --bundles nsis @extraConfig -- --locked }
} finally {
    $env:CARGO_TARGET_DIR = $saved.CARGO_TARGET_DIR
    $env:CARGO_INCREMENTAL = $saved.CARGO_INCREMENTAL
    Pop-Location
}

$installer = Join-Path $targetDir "$target/release/bundle/nsis/${product}_${version}_x64-setup.exe"
if (-not (Test-Path $installer)) { throw "Missing installer: $installer" }
if ($env:WINDOWS_CERTIFICATE_THUMBPRINT) {
    $signature = Get-AuthenticodeSignature $installer
    if ($signature.Status -ne 'Valid') { throw "The installer signature is not valid: $($signature.Status)" }
}

$name = "mmx-wallet_${version}_x64-setup.exe"
$publish = Join-Path $packages ('.publish-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $publish | Out-Null
try {
    Copy-Item $installer (Join-Path $publish $name)
    $hash = (Get-FileHash (Join-Path $publish $name) -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-Utf8 (Join-Path $publish 'sha256sums-windows-x64') "$hash  $name`n"
    Get-ChildItem $publish | ForEach-Object { Move-Item $_.FullName (Join-Path $packages $_.Name) -Force }
} finally {
    Remove-Item $publish -Recurse -Force -ErrorAction SilentlyContinue
}
Get-Item (Join-Path $packages $name) | Format-Table Name, Length
