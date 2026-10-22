param(
    [string[]]$Path = @(),
    [switch]$RequireSignature,
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'

$encodedCertificate = $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64
$certificatePassword = $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD
[Environment]::SetEnvironmentVariable('OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64', $null, 'Process')
[Environment]::SetEnvironmentVariable('OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD', $null, 'Process')
if ([string]::IsNullOrWhiteSpace($encodedCertificate)) {
    if (-not [string]::IsNullOrWhiteSpace($certificatePassword)) {
        throw 'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD is configured without OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64.'
    }
    if ($RequireSignature) {
        throw 'Authenticode signing is required, but OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 is not configured.'
    }
    Write-Host 'Authenticode signing is not configured; continuing with unsigned Windows binaries.'
    return
}
if (-not $ValidateOnly -and $Path.Count -eq 0) {
    throw 'Provide at least one artifact to sign unless ValidateOnly is requested.'
}
if ($null -eq $certificatePassword) {
    throw 'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD must be configured (it may be empty).'
}
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    throw 'RUNNER_TEMP must be set so the temporary PFX can be isolated and removed.'
}

$pfxPath = Join-Path $env:RUNNER_TEMP ("openshazam-signing-{0}.pfx" -f [guid]::NewGuid().ToString('N'))
$certificate = $null
try {
    try {
        $pfxBytes = [Convert]::FromBase64String($encodedCertificate)
    }
    catch {
        throw 'OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 is not valid base64.'
    }
    try {
        [IO.File]::WriteAllBytes($pfxPath, $pfxBytes)
    }
    finally {
        # Keep the decoded secret short-lived even when the temporary-file write fails.
        [Array]::Clear($pfxBytes, 0, $pfxBytes.Length)
    }

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
    $basicConstraints = $certificate.Extensions |
        Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509BasicConstraintsExtension] } |
        Select-Object -First 1
    if (-not $basicConstraints) {
        throw 'The configured signing certificate must include a Basic Constraints extension identifying a non-CA certificate.'
    }
    if ($basicConstraints.CertificateAuthority) {
        throw 'The configured signing certificate is a certificate authority, not a code-signing leaf certificate.'
    }
    $keyUsage = $certificate.Extensions |
        Where-Object { $_ -is [Security.Cryptography.X509Certificates.X509KeyUsageExtension] } |
        Select-Object -First 1
    if (-not $keyUsage -or
        ($keyUsage.KeyUsages -band [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature) -eq 0) {
        throw 'The configured signing certificate must permit digital signatures.'
    }
    $hasCodeSigningEku = $false
    foreach ($extension in $certificate.Extensions) {
        if ($extension -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension] -and
            ($extension.EnhancedKeyUsages | Where-Object { $_.Value -eq '1.3.6.1.5.5.7.3.3' })) {
            $hasCodeSigningEku = $true
            break
        }
    }
    if (-not $hasCodeSigningEku) {
        throw 'The configured certificate does not include the Code Signing enhanced key usage.'
    }

    if ($ValidateOnly) {
        Write-Host 'Authenticode certificate is valid for code signing.'
        return
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
        $signature = Get-AuthenticodeSignature -LiteralPath $resolvedPath
        if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
            -not $signature.SignerCertificate -or
            $signature.SignerCertificate.Thumbprint -ne $certificate.Thumbprint) {
            throw "Authenticode signature verification did not identify the configured certificate for $resolvedPath."
        }
        Write-Host "Signed and verified: $resolvedPath"
    }
}
finally {
    try {
        if ($certificate) { $certificate.Dispose() }
        if (Test-Path -LiteralPath $pfxPath) {
            Remove-Item -LiteralPath $pfxPath -Force
        }
    }
    finally {
        $encodedCertificate = $null
        $certificatePassword = $null
    }
}
