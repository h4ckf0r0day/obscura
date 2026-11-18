param(
    [string]$StageDirectory,
    [string]$ArchivePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-WindowsDistributionTools {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or !$vs) { throw 'MSVC installation not found' }
    $tools = Get-ChildItem "$vs\VC\Tools\MSVC" -Directory |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
    $redist = Get-ChildItem "$vs\VC\Redist\MSVC" -Directory |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
    $crt = @(Get-ChildItem "$($redist.FullName)\x64\Microsoft.VC*.CRT" -Directory)
    if ($crt.Count -ne 1) { throw 'Expected one x64 MSVC release CRT directory' }
    return @{
        Dumpbin = Join-Path $tools.FullName 'bin\Hostx64\x64\dumpbin.exe'
        Redist = $crt[0].FullName
    }
}

function ConvertFrom-PeDependencies([string[]]$Output) {
    $dependencies = @($Output | ForEach-Object {
        # /DEPENDENTS includes ordinary and delay-load imports.
        if ("$_" -match '^\s+(.+?\.dll)\s*$') {
            $name = $Matches[1].ToLowerInvariant()
            if ($name -notmatch '^[\w.-]+\.dll$') { throw "Invalid DLL import: $name" }
            $name
        }
    } | Sort-Object -Unique)
    if (!$dependencies.Count) { throw 'No DLL imports parsed' }
    return $dependencies
}

function Get-PeDependencies([string]$Path, [string]$Dumpbin) {
    $output = & $Dumpbin /DEPENDENTS $Path 2>&1
    if ($LASTEXITCODE -ne 0) { throw "dumpbin failed for ${Path}: $output" }
    $dependencies = @(ConvertFrom-PeDependencies $output)
    Write-Host "DEPENDENTS ${Path}: $($dependencies -join ', ')"
    return $dependencies
}

function Test-WindowsSystemDll([string]$Name) {
    # Windows 10/11 supply the UCRT and API-set contracts, not the VC++ runtime.
    return $Name -match '^(api-ms-win-|ext-ms-win-)[\w.-]+\.dll$' -or $Name -in @(
        'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'crypt32.dll',
        'dbghelp.dll', 'gdi32.dll', 'iphlpapi.dll', 'kernel32.dll', 'kernelbase.dll',
        'msvcrt.dll', 'ncrypt.dll', 'ntdll.dll', 'ole32.dll', 'oleaut32.dll',
        'powrprof.dll', 'propsys.dll', 'rpcrt4.dll', 'secur32.dll', 'shell32.dll',
        'shlwapi.dll', 'ucrtbase.dll', 'user32.dll', 'userenv.dll', 'version.dll',
        'winhttp.dll', 'winmm.dll', 'ws2_32.dll'
    )
}

function Resolve-WindowsDependencies([string]$Directory, [string]$Dumpbin, [string]$Redist = '') {
    foreach ($name in @('obscura.exe', 'obscura-worker.exe')) {
        if (!(Test-Path -LiteralPath (Join-Path $Directory $name) -PathType Leaf)) {
            throw "Missing packaged executable: $name"
        }
    }
    $queue = [Collections.Generic.Queue[string]]::new()
    Get-ChildItem -LiteralPath $Directory -File | Where-Object Extension -in '.exe', '.dll' |
        ForEach-Object { $queue.Enqueue($_.FullName) }
    $visited = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    while ($queue.Count) {
        $path = $queue.Dequeue()
        if (!$visited.Add($path)) { continue }
        foreach ($name in @(Get-PeDependencies $path $Dumpbin)) {
            $local = Join-Path $Directory $name
            if (Test-Path -LiteralPath $local -PathType Leaf) {
                $queue.Enqueue($local)
            } elseif (Test-WindowsSystemDll $name) {
                continue
            } elseif ($Redist -and $name -match '^(vcruntime140(?:_\d+)?|msvcp140(?:_\w+)?|concrt140)\.dll$') {
                # Only Microsoft's release redistributables, never System32 or debug DLLs.
                # https://learn.microsoft.com/cpp/windows/redistributing-visual-cpp-files
                $source = Join-Path $Redist $name
                if (!(Test-Path -LiteralPath $source -PathType Leaf)) { throw "Redistributable missing: $name" }
                Copy-Item -LiteralPath $source -Destination $local
                Write-Host "APP-LOCAL $source -> $local"
                $queue.Enqueue($local)
            } else {
                throw "Unbundled non-system dependency: $name (from $path)"
            }
        }
    }
}

function Test-WindowsPackageStartup([string]$Directory) {
    # These are runner smoke tests, not evidence of a clean Windows installation.
    foreach ($name in @('obscura.exe', 'obscura-worker.exe')) {
        $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $Directory $name))
        $info.UseShellExecute = $false
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.CreateNoWindow = $true
        $info.WorkingDirectory = $Directory
        if ($name -eq 'obscura.exe') {
            foreach ($arg in @('fetch', 'data:text/html,<title>runtime</title>', '--eval', '1+1')) {
                $info.ArgumentList.Add($arg)
            }
        }
        $process = [Diagnostics.Process]::Start($info)
        $stderr = $process.StandardError.ReadToEndAsync()
        if ($name -eq 'obscura-worker.exe') {
            $process.StandardInput.WriteLine('{"cmd":"navigate","url":"data:text/html,<title>runtime</title>"}')
            $process.StandardInput.WriteLine('{"cmd":"evaluate","expression":"1+1"}')
            $responses = @(foreach ($index in 0..1) {
                $line = $process.StandardOutput.ReadLineAsync()
                if (!$line.Wait(60000)) {
                    $process.Kill($true)
                    throw 'Worker response timed out'
                }
                $line.GetAwaiter().GetResult() | ConvertFrom-Json
            })
            # Keep stdin open while checking the live process. A successful
            # launch alone could have used a CRT already installed on the runner.
            $modules = @($process.Modules)
            foreach ($dll in Get-ChildItem -LiteralPath $Directory -Filter '*.dll') {
                $loaded = @($modules | Where-Object ModuleName -eq $dll.Name)
                if ($loaded.Count -ne 1 -or $loaded[0].FileName -ne $dll.FullName) {
                    throw "Worker did not load app-local $($dll.Name)"
                }
                Write-Host "LOADED APP-LOCAL $($loaded[0].FileName)"
            }
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $process.StandardInput.Close()
        if (!$process.WaitForExit(60000)) {
            $process.Kill($true)
            throw "$name startup timed out"
        }
        $out = $stdout.GetAwaiter().GetResult()
        $err = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "$name exited $($process.ExitCode): $err" }
        if ($name -eq 'obscura-worker.exe') {
            if ($responses.Count -ne 2 -or $responses[0].ok -ne $true -or
                $responses[1].ok -ne $true -or $responses[1].result -ne 2) {
                throw "Worker runtime smoke failed: $($responses | ConvertTo-Json -Compress) $err"
            }
        } elseif (($out | ConvertFrom-Json) -ne 2) { throw "CLI runtime smoke failed: $out $err" }
        Write-Host "STARTUP PASS $name"
        $process.Dispose()
    }
}

function New-WindowsPackage([string]$Directory, [string]$Archive) {
    $Directory = (Resolve-Path -LiteralPath $Directory).Path
    $tools = Get-WindowsDistributionTools
    Resolve-WindowsDependencies $Directory $tools.Dumpbin $tools.Redist
    if (Test-Path -LiteralPath $Archive) { throw "Archive already exists: $Archive" }
    Compress-Archive -Path "$Directory\*" -DestinationPath $Archive
    $extract = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
    try {
        Expand-Archive -LiteralPath $Archive -DestinationPath $extract
        # Verification cannot consult the redist directory or the runner's PATH.
        Resolve-WindowsDependencies $extract $tools.Dumpbin
        Test-WindowsPackageStartup $extract
    } finally {
        if (Test-Path -LiteralPath $extract) { Remove-Item -LiteralPath $extract -Recurse -Force }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    if (!$StageDirectory -or !$ArchivePath) { throw 'StageDirectory and ArchivePath are required' }
    New-WindowsPackage $StageDirectory $ArchivePath
}
