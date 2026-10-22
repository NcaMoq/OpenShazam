$ErrorActionPreference = 'Stop'
$sourceRoot = Join-Path (Split-Path -Parent $PSScriptRoot) 'apps/openshazam/src'
$sourceFiles = @(Get-ChildItem -LiteralPath $sourceRoot -File -Filter '*.rs' -Recurse)
$unsafeFiles = 0
$violations = [System.Collections.Generic.List[string]]::new()

foreach ($file in $sourceFiles) {
    $source = [IO.File]::ReadAllText($file.FullName, [Text.Encoding]::UTF8)
    if ($source -notmatch '\bunsafe\s*(?:\{|\b(?:fn|extern|impl|trait)\b)') {
        continue
    }

    $unsafeFiles++
    if ($source -notmatch '(?m)^#!\[deny\(unsafe_op_in_unsafe_fn\)\]\r?$') {
        $relativePath = [IO.Path]::GetRelativePath($sourceRoot, $file.FullName).Replace('\', '/')
        $violations.Add($relativePath)
    }
}

if ($violations.Count -gt 0) {
    throw "Rust files containing unsafe operations must deny unsafe_op_in_unsafe_fn: $($violations -join ', ')"
}
if ($unsafeFiles -eq 0) {
    throw 'The unsafe policy scan did not find any Rust source files to validate.'
}

Write-Host "Unsafe-operation lint policy passed for $unsafeFiles Rust source files."
