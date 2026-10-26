# Run on both real Windows architectures before an update is published.
$ErrorActionPreference = 'Stop'
$setup = (Get-ChildItem release/*-setup.exe | Select-Object -First 1).FullName
$dir = Join-Path $env:LOCALAPPDATA 'spark-rs'

function Install-Spark([string[]] $Flags) {
    Write-Host "Installing with $($Flags -join ' ')"
    $process = Start-Process -FilePath $setup -ArgumentList $Flags -Wait -PassThru
    Write-Host "Installer exited $($process.ExitCode)"
    if ($process.ExitCode -ne 0) { throw "Installer exited $($process.ExitCode)" }
}

Install-Spark -Flags @('/S')
foreach ($file in @('spark-desktop.exe', 'spark-cli.exe', 'uninstall.exe')) {
    if (!(Test-Path (Join-Path $dir $file))) { throw "Missing installed $file" }
}
$shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'spark-rs.lnk'
if (!(Test-Path $shortcut)) { throw 'Start menu shortcut was not created' }
& "$dir/spark-cli.exe" doctor
if ($LASTEXITCODE -ne 0) { throw "Installed doctor failed: $LASTEXITCODE" }

# Exercise the credential cleanup entry point without opening the app. These
# are disposable fixtures on the hosted runner, never real sign-ins.
& cmdkey /generic:spark-installer-smoke.spark-rs /user:fixture /pass:fixture
if ($LASTEXITCODE -ne 0) { throw 'Could not create the app credential fixture' }
& cmdkey /generic:spark-installer-smoke-other /user:fixture /pass:fixture
if ($LASTEXITCODE -ne 0) { throw 'Could not create the unrelated credential fixture' }
$forget = Start-Process "$dir/spark-desktop.exe" -ArgumentList '--forget-saved-passwords' -Wait -PassThru
if ($forget.ExitCode -ne 0) { throw "Credential cleanup exited $($forget.ExitCode)" }
$credentials = (& cmdkey /list) -join "`n"
if ($credentials.Contains('spark-installer-smoke.spark-rs')) { throw 'App credential survived cleanup' }
if (!$credentials.Contains('spark-installer-smoke-other')) { throw 'Cleanup removed an unrelated credential' }
& cmdkey /delete:spark-installer-smoke-other
Write-Host 'Credential cleanup passed'

# A real MCP transport keeps the installed CLI open. Reinstalling in update
# mode must release that file, including when the agent is waiting on stdin.
$start = [Diagnostics.ProcessStartInfo]::new("$dir/spark-cli.exe", 'mcp')
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$agent = [Diagnostics.Process]::Start($start)
Write-Host "Started installed CLI $($agent.Id), sharing the test shell's console"
$otherDir = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
New-Item -ItemType Directory $otherDir | Out-Null
Copy-Item "$dir/spark-cli.exe" "$otherDir/spark-cli.exe"
$start.FileName = "$otherDir/spark-cli.exe"
$otherAgent = [Diagnostics.Process]::Start($start)
try {
    Start-Sleep -Seconds 2
    if ($agent.HasExited) { throw 'MCP process did not stay open for the replacement check' }
    $data = Join-Path $env:LOCALAPPDATA 'dev.sparkrs.desktop'
    New-Item -ItemType Directory -Force $data | Out-Null
    $sentinel = Join-Path $data 'installer-smoke.txt'
    Set-Content $sentinel 'Keep mail data during updates and ordinary uninstalls'
    Install-Spark -Flags @('/S', '/UPDATE')
    Write-Host 'Update returned without terminating the parent shell'
    if (!$agent.WaitForExit(10000)) { throw 'Update did not end the installed CLI' }
    if ($otherAgent.HasExited) { throw 'Update ended a CLI from another folder' }
    if (!(Test-Path $sentinel)) { throw 'Update deleted app data' }
    & "$dir/spark-cli.exe" doctor
    if ($LASTEXITCODE -ne 0) { throw "Updated doctor failed: $LASTEXITCODE" }
    Get-Process spark-desktop -ErrorAction SilentlyContinue | Stop-Process -Force
    $uninstall = Start-Process "$dir/uninstall.exe" -ArgumentList '/S' -Wait -PassThru
    if ($uninstall.ExitCode -ne 0) { throw "Uninstall exited $($uninstall.ExitCode)" }
    if (!(Test-Path $sentinel)) { throw 'Ordinary uninstall deleted app data' }
    Write-Host 'Install, CLI replacement and uninstall checks passed'
} finally {
    if (!$agent.HasExited) { $agent.Kill() }
    $agent.Dispose()
    if (!$otherAgent.HasExited) { $otherAgent.Kill(); $otherAgent.WaitForExit() }
    $otherAgent.Dispose()
    Remove-Item $otherDir -Recurse -Force
}
