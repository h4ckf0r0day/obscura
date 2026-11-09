Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/package_windows.ps1"

function Assert-Fails([scriptblock]$Action, [string]$Expected) {
    try { & $Action } catch {
        if ($_.Exception.Message -notlike "*$Expected*") { throw }
        return
    }
    throw "Expected failure containing: $Expected"
}

$parsed = @(ConvertFrom-PeDependencies @(
    '    KERNEL32.dll', '  Image has the following delay load dependencies:',
    '    VCRUNTIME140_1.dll', '        1000 .rdata', '    KERNEL32.dll'
))
if ($parsed.Count -ne 2 -or $parsed -notcontains 'vcruntime140_1.dll') { throw 'Import parser failed' }
Assert-Fails { ConvertFrom-PeDependencies @('unrecognized dumpbin output') } 'No DLL imports'
Assert-Fails { ConvertFrom-PeDependencies @('    ..\outside.dll') } 'Invalid DLL import'
foreach ($name in @('kernel32.dll', 'ucrtbase.dll', 'api-ms-win-crt-runtime-l1-1-0.dll')) {
    if (!(Test-WindowsSystemDll $name)) { throw "System DLL rejected: $name" }
}
foreach ($name in @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll', 'unknown.dll')) {
    if (Test-WindowsSystemDll $name) { throw "Non-system DLL accepted: $name" }
}

# Model transitive imports independently of DLLs preinstalled on the runner.
function Get-PeDependencies([string]$Path, [string]$Dumpbin) {
    return $script:imports[[IO.Path]::GetFileName($Path)]
}
$root = Join-Path $env:RUNNER_TEMP ([Guid]::NewGuid().ToString())
$stage = Join-Path $root 'stage'
$redist = Join-Path $root 'redist'
New-Item -ItemType Directory -Path $stage, $redist | Out-Null
foreach ($name in @('obscura.exe', 'obscura-worker.exe')) { Set-Content (Join-Path $stage $name) 'fixture' }
foreach ($name in @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    Set-Content (Join-Path $redist $name) 'redistributable fixture'
}
$script:imports = @{
    'obscura.exe' = @('kernel32.dll', 'msvcp140.dll')
    'obscura-worker.exe' = @('kernel32.dll', 'vcruntime140_1.dll')
    'msvcp140.dll' = @('vcruntime140.dll', 'vcruntime140_1.dll')
    'vcruntime140.dll' = @('kernel32.dll', 'api-ms-win-crt-runtime-l1-1-0.dll')
    'vcruntime140_1.dll' = @('vcruntime140.dll')
}
Assert-Fails { Resolve-WindowsDependencies $stage 'unused' } 'Unbundled non-system dependency'
Resolve-WindowsDependencies $stage 'unused' $redist
Resolve-WindowsDependencies $stage 'unused'
$script:imports['vcruntime140.dll'] += 'vcruntime140_1.dll'
Resolve-WindowsDependencies $stage 'unused'
foreach ($name in @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    if (!(Test-Path (Join-Path $stage $name))) { throw "Transitive runtime missing: $name" }
}
Remove-Item (Join-Path $stage 'vcruntime140.dll')
Assert-Fails { Resolve-WindowsDependencies $stage 'unused' } 'vcruntime140.dll'
Resolve-WindowsDependencies $stage 'unused' $redist
$script:imports['vcruntime140.dll'] = @('unexpected.dll')
Assert-Fails { Resolve-WindowsDependencies $stage 'unused' $redist } 'unexpected.dll'
$script:imports['vcruntime140.dll'] = @('vcruntime140d.dll')
Assert-Fails { Resolve-WindowsDependencies $stage 'unused' $redist } 'vcruntime140d.dll'
Remove-Item (Join-Path $stage 'obscura-worker.exe')
Assert-Fails { Resolve-WindowsDependencies $stage 'unused' } 'Missing packaged executable'
Write-Host 'Windows packaging helper regression checks passed'
