$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = '1.8.260317003'
$expectedSha256 = '1daf6a5537c0e4785df85964e42299404e9f7744491c3b84b24b65fb94e49acf'
$relativeOutput = 'target/windows-app-sdk-runtime/WindowsAppRuntimeInstall-x64.exe'
$output = Join-Path $root $relativeOutput
$outputDirectory = Split-Path -Parent $output
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null

$signature = $null
$existingHash = if (Test-Path -LiteralPath $output -PathType Leaf) {
    (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
}
if ($existingHash -cne $expectedSha256) {
    $uri = "https://aka.ms/windowsappsdk/1.8/$version/windowsappruntimeinstall-x64.exe"
    Invoke-WebRequest -Uri $uri -OutFile $output
}
$signature = Get-AuthenticodeSignature -LiteralPath $output
$actualHash = (Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -cne $expectedSha256) {
    throw "The Windows App SDK runtime SHA-256 does not match the pinned $version binary."
}
if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
    throw "The Windows App SDK runtime installer signature is invalid: $($signature.StatusMessage)"
}
if ($signature.SignerCertificate.Subject -notmatch '(^|,\s*)CN=Microsoft Corporation(,|$)') {
    throw "The Windows App SDK runtime installer is not signed by Microsoft: $($signature.SignerCertificate.Subject)"
}
Write-Host "Verified Microsoft Windows App SDK runtime ${version}: $output"
