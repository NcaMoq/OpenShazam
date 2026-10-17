<#
.SYNOPSIS
Captures a Windows UI performance trace and per-process resource samples.

.DESCRIPTION
Open the OpenShazam popup before running this script. It records GPU and
Desktop Composition events with WPR, plus CPU/memory/GPU process counters.
CPU/GPU percentile values describe resource samples; frame interval p95/p99
must be derived from the ETL in Windows Performance Analyzer (WPA).
Use -ResourceOnly when WPR elevation is unavailable; this samples process
resources without collecting ETW or frame timing.

.EXAMPLE
$app = Get-Process -Name openshazam | Select-Object -First 1
.\tools\profile-windows-ui.ps1 -ProcessId $app.Id -DurationSeconds 30

.EXAMPLE
$app = Get-Process -Name openshazam | Select-Object -First 1
.\tools\profile-windows-ui.ps1 -ProcessId $app.Id -DurationSeconds 30 -ResourceOnly
#>
param(
    [int]$ProcessId = 0,
    [ValidateRange(5, 600)]
    [int]$DurationSeconds = 30,
    [switch]$ResourceOnly,
    [string]$OutputDirectory = (Join-Path (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\windows-x64') 'profiles')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$distRoot = [IO.Path]::GetFullPath((Join-Path $root 'dist\windows-x64')).TrimEnd('\')
$resolvedOutputDirectory = if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    [IO.Path]::GetFullPath($OutputDirectory)
}
else {
    [IO.Path]::GetFullPath((Join-Path $distRoot $OutputDirectory))
}
$distPrefix = $distRoot + [IO.Path]::DirectorySeparatorChar
if (-not $resolvedOutputDirectory.Equals($distRoot, [StringComparison]::OrdinalIgnoreCase) -and
    -not $resolvedOutputDirectory.StartsWith($distPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Profile output is restricted to $distRoot"
}
$OutputDirectory = $resolvedOutputDirectory

$source = @'
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class OpenShazamProfileWindowProbe
{
    private delegate bool EnumWindowsCallback(IntPtr hwnd, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsCallback callback, IntPtr parameter);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassNameW(IntPtr hwnd, StringBuilder name, int maximum);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    public static bool HasVisibleMainWindow(int processId)
    {
        bool found = false;
        EnumWindows((hwnd, _) =>
        {
            uint ownerProcessId;
            GetWindowThreadProcessId(hwnd, out ownerProcessId);
            if (ownerProcessId != (uint)processId || !IsWindowVisible(hwnd)) return true;
            var className = new StringBuilder(256);
            GetClassNameW(hwnd, className, className.Capacity);
            if (className.ToString() == "OpenShazamNativeUiWindow")
            {
                found = true;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@
if (-not ("OpenShazamProfileWindowProbe" -as [type])) {
    Add-Type -TypeDefinition $source -ErrorAction Stop
}

if ($ProcessId -eq 0) {
    $matchingProcesses = @(Get-Process -Name 'openshazam' -ErrorAction SilentlyContinue)
    if ($matchingProcesses.Count -ne 1) {
        throw 'Start OpenShazam, open its popup, then pass its PID with -ProcessId. Exactly one OpenShazam process must be running.'
    }
    $ProcessId = $matchingProcesses[0].Id
}

$targetProcess = Get-Process -Id $ProcessId -ErrorAction Stop
if ($targetProcess.ProcessName -ne 'openshazam') {
    throw "PID $ProcessId is '$($targetProcess.ProcessName)', not OpenShazam."
}
if (-not [OpenShazamProfileWindowProbe]::HasVisibleMainWindow($ProcessId)) {
    throw 'The OpenShazam popup is not visible. Open the popup before starting this profile.'
}

$wpr = $null
if (-not $ResourceOnly) {
    $wpr = Get-Command 'wpr.exe' -ErrorAction SilentlyContinue
    if (-not $wpr) { throw 'Windows Performance Recorder (wpr.exe) is not installed.' }
}
$null = New-Item -ItemType Directory -Path $OutputDirectory -Force
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory -ErrorAction Stop).Path
if (-not $OutputDirectory.Equals($distRoot, [StringComparison]::OrdinalIgnoreCase) -and
    -not $OutputDirectory.StartsWith($distPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Resolved profile output escaped $distRoot"
}
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$tracePath = Join-Path $OutputDirectory "openshazam-$stamp.etl"
$csvPath = Join-Path $OutputDirectory "openshazam-$stamp-samples.csv"
$jsonPath = Join-Path $OutputDirectory "openshazam-$stamp-summary.json"
$logicalProcessorCount = [Environment]::ProcessorCount
$samples = [System.Collections.Generic.List[object]]::new()
$counterWarnings = [System.Collections.Generic.List[string]]::new()
$recordingStarted = $false
$recordingStopped = $false

function Get-Percentile([double[]]$Values, [double]$Percentile) {
    if (-not $Values -or $Values.Count -eq 0) { return $null }
    $ordered = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Ceiling($Percentile * $ordered.Count) - 1)
    return [double]$ordered[$index]
}

function Get-TargetCounterValue($CounterSamples, [string]$SetName, [int]$TargetPid, [string]$CounterName) {
    $needle = "(pid_${TargetPid}_"
    $pattern = "*\$SetName$needle*\$CounterName"
    $values = @($CounterSamples | Where-Object {
        $_.Path -like $pattern
    } | ForEach-Object { [double]$_.CookedValue })
    if ($values.Count -eq 0) { return $null }
    return ($values | Measure-Object -Sum).Sum
}

try {
    if (-not $ResourceOnly) {
        & $wpr.Source -start GPU -start DesktopComposition -filemode
        if ($LASTEXITCODE -ne 0) {
            throw "WPR could not start GPU/DesktopComposition recording (exit $LASTEXITCODE). Run in an elevated PowerShell window if required, or use -ResourceOnly for process counters."
        }
        $recordingStarted = $true
    }

    $sampleIntervalSeconds = 1.0
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $previousCpuSeconds = (Get-Process -Id $ProcessId -ErrorAction Stop).CPU
    $previousElapsed = $watch.Elapsed.TotalSeconds
    Start-Sleep -Milliseconds ([int]($sampleIntervalSeconds * 1000.0))
    while ($watch.Elapsed.TotalSeconds -lt $DurationSeconds) {
        $now = $watch.Elapsed.TotalSeconds
        $proc = Get-Process -Id $ProcessId -ErrorAction Stop
        $elapsed = [Math]::Max(0.001, $now - $previousElapsed)
        $cpuDelta = [Math]::Max(0.0, $proc.CPU - $previousCpuSeconds)
        $cpuMachinePercent = 100.0 * $cpuDelta / ($elapsed * $logicalProcessorCount)
        $previousCpuSeconds = $proc.CPU
        $previousElapsed = $now

        $gpuEngine = $null
        $gpuCommitted = $null
        try {
            $counterSamples = (Get-Counter -Counter @(
                "\GPU Engine(pid_${ProcessId}_*)\Utilization Percentage",
                "\GPU Process Memory(pid_${ProcessId}_*)\Total Committed"
            ) -MaxSamples 1).CounterSamples
            $gpuEngine = Get-TargetCounterValue $counterSamples 'GPU Engine' $ProcessId 'Utilization Percentage'
            $gpuCommitted = Get-TargetCounterValue $counterSamples 'GPU Process Memory' $ProcessId 'Total Committed'
        }
        catch {
            if (-not $counterWarnings.Contains('GPU Engine counters unavailable: ' + $_.Exception.Message)) {
                $counterWarnings.Add('GPU Engine counters unavailable: ' + $_.Exception.Message)
            }
            if (-not $counterWarnings.Contains('GPU Process Memory counters unavailable: ' + $_.Exception.Message)) {
                $counterWarnings.Add('GPU Process Memory counters unavailable: ' + $_.Exception.Message)
            }
        }

        $samples.Add([pscustomobject]@{
            ElapsedSeconds = [Math]::Round($now, 3)
            CpuPercentOfMachine = [Math]::Round($cpuMachinePercent, 3)
            WorkingSetBytes = [long]$proc.WorkingSet64
            PrivateBytes = [long]$proc.PrivateMemorySize64
            GpuEngineUtilizationSumPercent = if ($null -eq $gpuEngine) { $null } else { [Math]::Round($gpuEngine, 3) }
            GpuProcessCommittedBytes = if ($null -eq $gpuCommitted) { $null } else { [long]$gpuCommitted }
        })

        # Query latency varies with the Windows performance-counter provider.
        # Keep the intended cadence near one second without adding a full
        # second of sleep after every counter read.
        $remaining = $sampleIntervalSeconds - ($watch.Elapsed.TotalSeconds - $now)
        if ($remaining -gt 0) {
            Start-Sleep -Milliseconds ([int][Math]::Ceiling($remaining * 1000.0))
        }
    }

    if ($recordingStarted) {
        & $wpr.Source -stop $tracePath 'OpenShazam Windows UI GPU and Desktop Composition profile' -skipPdbGen -compress
        if ($LASTEXITCODE -ne 0) { throw "WPR could not save the ETL trace (exit $LASTEXITCODE)." }
        $recordingStopped = $true
    }

    $samples | Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8
    $cpuValues = [double[]]@($samples | ForEach-Object { $_.CpuPercentOfMachine })
    $gpuValues = [double[]]@($samples | Where-Object { $null -ne $_.GpuEngineUtilizationSumPercent } | ForEach-Object { $_.GpuEngineUtilizationSumPercent })
    $workingSetValues = [double[]]@($samples | ForEach-Object { $_.WorkingSetBytes })
    $privateValues = [double[]]@($samples | ForEach-Object { $_.PrivateBytes })
    $gpuMemoryValues = [double[]]@($samples | Where-Object { $null -ne $_.GpuProcessCommittedBytes } | ForEach-Object { $_.GpuProcessCommittedBytes })

    $summary = [pscustomobject]@{
        ProcessId = $ProcessId
        DurationSeconds = $DurationSeconds
        SampleCount = $samples.Count
        CpuPercentOfMachine = [pscustomobject]@{
            Average = if ($cpuValues.Count) { [Math]::Round(($cpuValues | Measure-Object -Average).Average, 3) } else { $null }
            P95 = Get-Percentile $cpuValues 0.95
            P99 = Get-Percentile $cpuValues 0.99
        }
        GpuEngineUtilizationSumPercent = [pscustomobject]@{
            Average = if ($gpuValues.Count) { [Math]::Round(($gpuValues | Measure-Object -Average).Average, 3) } else { $null }
            P95 = Get-Percentile $gpuValues 0.95
            P99 = Get-Percentile $gpuValues 0.99
            Note = 'Sum across this process GPU engine instances; can exceed 100% when engines overlap.'
        }
        PeakWorkingSetBytes = if ($workingSetValues.Count) { ($workingSetValues | Measure-Object -Maximum).Maximum } else { $null }
        PeakPrivateBytes = if ($privateValues.Count) { ($privateValues | Measure-Object -Maximum).Maximum } else { $null }
        PeakGpuProcessCommittedBytes = if ($gpuMemoryValues.Count) { ($gpuMemoryValues | Measure-Object -Maximum).Maximum } else { $null }
        TracePath = if ($ResourceOnly) { $null } else { $tracePath }
        SamplesPath = $csvPath
        CounterWarnings = @($counterWarnings)
        ResourcePercentilesNote = if ($ResourceOnly) { 'Resource-only run: CPU/GPU P95/P99 are percentiles over approximately one-second process samples, not frame intervals. No ETW trace was collected. GPU values may be null when Windows exposes no matching counter instances for this PID.' } else { 'CPU/GPU P95/P99 are percentiles over approximately one-second resource samples, not frame intervals. GPU values may be null when Windows exposes no matching counter instances for this PID.' }
        FrameIntervalPercentiles = if ($ResourceOnly) { 'Unavailable: ResourceOnly mode does not collect ETW frame events.' } else { 'Not derived automatically; use the ETL DesktopComposition/GPU events in WPA for frame-level analysis.' }
    }
    $summary | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $jsonPath -Encoding UTF8
    $summary
    Write-Host "Summary: $jsonPath"
}
finally {
    if ($recordingStarted -and -not $recordingStopped) {
        & $wpr.Source -cancel
    }
}
