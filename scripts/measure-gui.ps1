param(
    [Parameter(Mandatory)][string]$Project,
    [Parameter(Mandatory)][string]$OutputPrefix,
    [string]$Program = "target/release/geemil-desktop.exe",
    [ValidateRange(2, 3600)][int]$OrbitSeconds = 120,
    [ValidateRange(100000, 20000000)][int]$PointBudget = 2000000,
    [string]$SmokeScript = "",
    [string]$SmokeDialog = ""
)
# PowerShell 7 / Windows. Runs a fixed-path orbit benchmark. UI frame intervals
# are logged by the app; this script samples process RAM and GPU process memory.
$ErrorActionPreference = "Stop"
$projectPath = (Resolve-Path -LiteralPath $Project).Path
$programPath = (Resolve-Path -LiteralPath $Program).Path
$prefixPath = [IO.Path]::GetFullPath($OutputPrefix)
if (Test-Path -LiteralPath ($prefixPath + '.png')) { throw "Screenshot already exists" }
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($prefixPath)) | Out-Null
$startInfo = [Diagnostics.ProcessStartInfo]::new($programPath)
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
foreach ($argument in @($projectPath, '--smoke-test', ($prefixPath + '.png'), '--smoke-orbit',
    '--smoke-orbit-seconds', $OrbitSeconds.ToString(), '--smoke-budget', $PointBudget.ToString())) {
    $startInfo.ArgumentList.Add($argument)
}
if ($SmokeScript) { $startInfo.ArgumentList.Add('--smoke-script'); $startInfo.ArgumentList.Add($SmokeScript) }
if ($SmokeDialog) { $startInfo.ArgumentList.Add('--smoke-dialog'); $startInfo.ArgumentList.Add($SmokeDialog) }
$process = [Diagnostics.Process]::Start($startInfo)
$stdout = $process.StandardOutput.ReadToEndAsync()
$stderr = $process.StandardError.ReadToEndAsync()
$peakWorking = 0L
$peakPrivate = 0L
$peakDedicated = 0L
$peakShared = 0L
$gpuSamples = 0
$watch = [Diagnostics.Stopwatch]::StartNew()
try {
    while (-not $process.HasExited) {
        $process.Refresh()
        $peakWorking = [Math]::Max($peakWorking, $process.WorkingSet64)
        $peakPrivate = [Math]::Max($peakPrivate, $process.PrivateMemorySize64)
        $gpu = @(Get-CimInstance -ClassName Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match ('^pid_' + $process.Id + '_') })
        if ($gpu.Count -gt 0) {
            $dedicated = ($gpu | Measure-Object -Property DedicatedUsage -Sum).Sum
            $shared = ($gpu | Measure-Object -Property SharedUsage -Sum).Sum
            $peakDedicated = [Math]::Max($peakDedicated, [long]$dedicated)
            $peakShared = [Math]::Max($peakShared, [long]$shared)
            $gpuSamples++
        }
        if ($watch.Elapsed.TotalSeconds -gt ($OrbitSeconds + 180)) { throw "GUI benchmark timed out" }
        Start-Sleep -Milliseconds 500
    }
    $process.WaitForExit()
    $output = $stdout.GetAwaiter().GetResult()
    $errorLog = $stderr.GetAwaiter().GetResult()
    [IO.File]::WriteAllText($prefixPath + '.stdout.log', $output)
    [IO.File]::WriteAllText($prefixPath + '.stderr.log', $errorLog)
    $metrics = [ordered]@{
        project = $projectPath; orbit_seconds = $OrbitSeconds; point_budget = $PointBudget;
        elapsed_seconds = $watch.Elapsed.TotalSeconds; exit_code = $process.ExitCode;
        sampled_peak_working_bytes = $peakWorking; sampled_peak_private_bytes = $peakPrivate;
        gpu_samples = $gpuSamples;
        sampled_peak_dedicated_gpu_bytes = $(if ($gpuSamples) { $peakDedicated } else { $null });
        sampled_peak_shared_gpu_bytes = $(if ($gpuSamples) { $peakShared } else { $null });
        gpu_counter = 'Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory';
        screenshot = $prefixPath + '.png'
    }
    $metrics | ConvertTo-Json | Set-Content -LiteralPath ($prefixPath + '.metrics.json') -Encoding utf8
    $metrics | ConvertTo-Json -Compress
    if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath ($prefixPath + '.png')) -or
        $errorLog -match 'Smoke test error:|Smoke test timed out:|Screenshot failed:') {
        throw "GUI benchmark failed; see $prefixPath.stderr.log"
    }
} finally {
    if (-not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
}
