$ErrorActionPreference = 'Stop'

# WinRT Composition and DispatcherQueue objects are thread/apartment scoped.
# Keep each native integration test in its own test process so a previous test's
# COM apartment teardown cannot affect the next Compositor activation.
$tests = @(
    'windows_ui::tests::composition_animates_ellipse_radius_and_sprite_stroke_without_a_window'
    'windows_ui::tests::composition_builds_the_native_no_match_icon_path'
    'windows_ui::tests::composition_inner_ripple_group_accepts_the_inverse_circle_clip'
    'windows_ui::tests::composition_result_overlay_uploads_pixels_and_resizes_at_fractional_dpi'
    'windows_ui::tests::composition_status_text_renders_transitions_and_resizes_at_fractional_dpi'
)

foreach ($test in $tests) {
    Write-Host "Running isolated Windows Composition test: $test"
    & cargo test --locked -p openshazam --bin openshazam $test -- --ignored --exact --test-threads=1
    if ($LASTEXITCODE -ne 0) {
        throw "Windows Composition test failed: $test (exit code $LASTEXITCODE)"
    }
}
