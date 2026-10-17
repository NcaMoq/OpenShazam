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
    'composition_status_text_renders_transitions_and_resizes_at_fractional_dpi'
)

Push-Location $repoRoot
try {
    foreach ($testName in $compositionTests) {
        $testFilter = "windows_ui::tests::$testName"
        Write-Host "Running isolated Windows Composition test: $testFilter"
        & cargo +1.85.0 test --locked -p openshazam --bin openshazam $testFilter -- --ignored --exact --test-threads=1
        if ($LASTEXITCODE -ne 0) {
            throw "Windows Composition test failed: $testFilter (exit code $LASTEXITCODE)"
        }
    }
}
finally {
    Pop-Location
}

Write-Host 'All native Composition tests passed in isolated processes.'
