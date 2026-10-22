$ErrorActionPreference = 'Stop'

# WinRT Composition and DispatcherQueue objects are thread/apartment scoped.
# Keep every native integration test in its own process so COM teardown from a
# previous test cannot affect the next Compositor activation.
if ($env:OS -ne 'Windows_NT') {
    throw 'This verification requires Windows Composition and a Windows desktop session.'
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$compositionTests = @(
    'composition_animates_ellipse_radius_and_sprite_stroke_without_a_window'
    'composition_builds_the_native_no_match_icon_path'
    'composition_auto_shadow_uses_button_viewport_for_336pt_mask'
    'composition_inner_ripple_group_accepts_the_inverse_circle_clip'
    'composition_result_overlay_uploads_pixels_and_resizes_at_fractional_dpi'
    'composition_history_overlays_capture_insert_and_scroll_surfaces'
    'composition_status_text_renders_transitions_and_resizes_at_fractional_dpi'
)

$uiSourceRoot = Join-Path $repoRoot 'apps\openshazam\src'
$uiSourceFiles = @(
    Get-Item -LiteralPath (Join-Path $uiSourceRoot 'windows_ui.rs')
    Get-ChildItem -LiteralPath (Join-Path $uiSourceRoot 'windows_ui') -Filter '*.rs' -File
)
$uiSource = ($uiSourceFiles | ForEach-Object {
    Get-Content -LiteralPath $_.FullName -Raw
}) -join [Environment]::NewLine
$ignoredNativeTests = @(
    [regex]::Matches($uiSource, '(?ms)#\[test\]\s*#\[ignore[^\]]*\]\s*fn\s+(?<name>[A-Za-z_]\w*)\s*\(') |
        ForEach-Object { $_.Groups['name'].Value }
)
$uncoveredTests = @($ignoredNativeTests | Where-Object { $compositionTests -notcontains $_ })
$unknownTests = @($compositionTests | Where-Object { $ignoredNativeTests -notcontains $_ })
if ($uncoveredTests.Count -gt 0 -or $unknownTests.Count -gt 0) {
    throw "The isolated Composition test list is stale. Missing: $($uncoveredTests -join ', '); not ignored: $($unknownTests -join ', ')."
}

Push-Location $repoRoot
try {
    foreach ($testName in $compositionTests) {
        $testFilter = "windows_ui::tests::$testName"
        Write-Host "Running isolated Windows Composition test: $testFilter"
        # Windows PowerShell 5.1 can turn native stderr output into an
        # ErrorRecord. Cargo writes normal progress there, so use its process
        # exit code as the test result instead of treating that output as a
        # terminating PowerShell error.
        $previousErrorActionPreference = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $cargoOutput = @(
                & cargo +1.85.0 test --locked -p openshazam --bin openshazam $testFilter -- --ignored --exact --test-threads=1 2>&1
            )
            $cargoExitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        foreach ($outputLine in $cargoOutput) {
            if ($outputLine -is [System.Management.Automation.ErrorRecord]) {
                Write-Host $outputLine.Exception.Message
            }
            else {
                Write-Host $outputLine
            }
        }
        if ($cargoExitCode -ne 0) {
            throw "Windows Composition test failed: $testFilter (exit code $cargoExitCode)"
        }
    }
}
finally {
    Pop-Location
}

Write-Host 'All native Composition tests passed in isolated processes.'
