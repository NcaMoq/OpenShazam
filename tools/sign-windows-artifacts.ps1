param(
    [Parameter(Mandatory = $true)]
    [string[]]$Path
)

$ErrorActionPreference = 'Stop'

$encodedCertificate = $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64
$certificatePassword = $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD
if ([string]::IsNullOrWhiteSpace($encodedCertificate)) {
    if (-not [string]::IsNullOrWhiteSpace($certificatePassword)) {
        throw 'WINDOWS_SIGNING_CERTIFICATE_PASSWORD is configured without WINDOWS_SIGNING_CERTIFICATE_BASE64.'
    }
    Write-Host 'Authenticode signing is not configured; continuing with unsigned Windows binaries.'
    return
}
if ($null -eq $certificatePassword) {
    throw 'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD must be configured (it may be empty).'
}
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw 'RUNNER_TEMP must be set so the temporary PFX can be isolated and removed.'
}

$pfxPath = Join-Path $env:RUNNER_TEMP ("openshazam-signing-{0}.pfx" -f [guid]::NewGuid().ToString('N'))
try {
    try {
        $pfxBytes = [Convert]::FromBase64String($encodedCertificate)
    }
    catch {
        throw 'OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 is not valid base64.'
    }
    [IO.File]::WriteAllBytes($pfxPath, $pfxBytes)

    $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new(
        $pfxPath,
        $certificatePassword,
        [Security.Cryptography.X509Certificates.X509KeyStorageFlags]::EphemeralKeySet
    )
    if (-not $certificate.HasPrivateKey) {
        throw 'The configured signing certificate has no private key.'
    }
    $now = [DateTime]::UtcNow
    if ($now -lt $certificate.NotBefore.ToUniversalTime() -or $now -gt $certificate.NotAfter.ToUniversalTime()) {
        throw 'The configured signing certificate is outside its validity period.'
    }
    $eku = $certificate.Extensions | Where-Object {
        $_ -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]
    } | Select-Object -First 1
    if (-not $eku -or -not ($eku.EnhancedKeyUsages | Where-Object { $_.Value -eq '1.3.6.1.5.5.7.3.3' })) {
        throw 'The configured certificate does not include the Code Signing enhanced key usage.'
    }

    $kitsRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $signtool = Get-ChildItem -LiteralPath $kitsRoot -Directory -ErrorAction Stop |
        Sort-Object { try { [version]$_.Name } catch { [version]'0.0' } } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    if (-not $signtool) {
        throw "Windows SDK signtool.exe was not found under $kitsRoot."
    }

    foreach ($item in $Path) {
        $resolvedPath = (Resolve-Path -LiteralPath $item -ErrorAction Stop).Path
        if ([IO.Path]::GetExtension($resolvedPath) -notin @('.exe', '.dll')) {
            throw "Refusing to Authenticode-sign unsupported artifact: $resolvedPath"
        }
        & $signtool sign /fd SHA256 /td SHA256 /tr 'https://timestamp.digicert.com' /f $pfxPath /p $certificatePassword /d 'Shazam' $resolvedPath
        if ($LASTEXITCODE -ne 0) {
            throw "signtool sign failed for $resolvedPath with exit code $LASTEXITCODE."
        }
        & $signtool verify /pa /all /v $resolvedPath
        if ($LASTEXITCODE -ne 0) {
            throw "signtool verification failed for $resolvedPath with exit code $LASTEXITCODE."
        }
        Write-Host "Signed and verified: $resolvedPath"
    }
}
finally {
    if (Test-Path -LiteralPath $pfxPath) {
        Remove-Item -LiteralPath $pfxPath -Force
    }
}
