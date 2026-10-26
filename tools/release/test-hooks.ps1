# Focused native regression for stopping a CLI without stopping its parent
# console or a copy in another folder. Uses an existing CI installer.
$ErrorActionPreference = 'Stop'
$setup = (Get-ChildItem release/*-setup.exe | Select-Object -First 1).FullName
$install = Start-Process $setup -ArgumentList '/S' -Wait -PassThru
if ($install.ExitCode -ne 0) { throw "Installer exited $($install.ExitCode)" }
$dir = Join-Path $env:LOCALAPPDATA 'spark-rs'
$compiler = Join-Path ${env:ProgramFiles(x86)} 'NSIS/makensis.exe'
$hooks = (Resolve-Path "$PSScriptRoot/../../apps/desktop/src-tauri/installer/hooks.nsh").Path
$helper = Join-Path $PSScriptRoot 'stop-installed-cli.exe'
& $compiler "/DHOOKS_FILE=$hooks" "/DOUTPUT_FILE=$helper" "$PSScriptRoot/test-hooks.nsi"
if ($LASTEXITCODE -ne 0) { throw 'Hook harness did not compile' }

function Start-Mcp([string] $File) {
    $start = [Diagnostics.ProcessStartInfo]::new($File, 'mcp')
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    return [Diagnostics.Process]::Start($start)
}
$otherDir = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory $otherDir | Out-Null
Copy-Item "$dir/spark-cli.exe" "$otherDir/spark-cli.exe"
$installed = Start-Mcp "$dir/spark-cli.exe"
$other = Start-Mcp "$otherDir/spark-cli.exe"
try {
    Start-Sleep -Seconds 2
    if ($installed.HasExited -or $other.HasExited) { throw 'CLI fixture exited before the hook ran' }
    Write-Host "Testing installed CLI $($installed.Id) and unrelated CLI $($other.Id) in the parent's console"
    $stop = Start-Process $helper -ArgumentList @('/S', "/D=$dir") -Wait -PassThru
    if ($stop.ExitCode -ne 0) { throw "Stop hook exited $($stop.ExitCode)" }
    if (!$installed.WaitForExit(10000)) { throw 'Installed CLI survived the stop hook' }
    if ($other.HasExited) { throw 'Stop hook ended a CLI from another folder' }
    Write-Host 'Parent shell and unrelated CLI survived; installed CLI stopped'
} finally {
    foreach ($process in @($installed, $other)) {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
    Remove-Item $otherDir -Recurse -Force
}
