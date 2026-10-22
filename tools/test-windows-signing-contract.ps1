$ErrorActionPreference = 'Stop'
$signingScript = Join-Path $PSScriptRoot 'sign-windows-artifacts.ps1'
$environmentNames = @(
    'OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64',
    'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD',
    'RUNNER_TEMP'
)
$savedEnvironment = @{}
foreach ($name in $environmentNames) {
    $item = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    $savedEnvironment[$name] = @{
        Exists = $null -ne $item
        Value = if ($null -ne $item) { $item.Value } else { $null }
    }
}

$testTempDirectory = Join-Path ([IO.Path]::GetTempPath()) ("openshazam-signing-contract-{0}" -f [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testTempDirectory | Out-Null

function Invoke-SigningFailureCheck([string]$Name, [hashtable]$Parameters, [string]$ExpectedMessage, [string]$SecretProbe) {
    $failure = $null
    $output = @()
    try {
        $output = @(& $signingScript @Parameters *>&1)
    }
    catch {
        $failure = $_.Exception.Message
    }
    if ([string]::IsNullOrWhiteSpace($failure)) {
        throw "Signing contract case '$Name' unexpectedly succeeded."
    }
    if ($ExpectedMessage -and $failure -notlike "*$ExpectedMessage*") {
        throw "Signing contract case '$Name' failed for an unexpected reason."
    }
    $transcript = (($output | ForEach-Object { $_.ToString() }) -join "`n") + "`n" + $failure
    if ($SecretProbe -and $transcript.Contains($SecretProbe)) {
        throw "Signing contract case '$Name' exposed a credential value in its output."
    }
    Assert-SigningCredentialsCleared $Name
}

function Invoke-SigningSuccessCheck([string]$Name, [hashtable]$Parameters, [string]$ExpectedMessage, [string]$SecretProbe) {
    $failure = $null
    $output = @()
    try {
        $output = @(& $signingScript @Parameters *>&1)
    }
    catch {
        $failure = $_.Exception.Message
    }
    if (-not [string]::IsNullOrWhiteSpace($failure)) {
        throw "Signing contract case '$Name' failed unexpectedly."
    }
    $transcript = ($output | ForEach-Object { $_.ToString() }) -join "`n"
    if (-not $transcript.Contains($ExpectedMessage)) {
        throw "Signing contract case '$Name' did not confirm the expected result."
    }
    if ($SecretProbe -and $transcript.Contains($SecretProbe)) {
        throw "Signing contract case '$Name' exposed a credential value in its output."
    }
    Assert-SigningCredentialsCleared $Name
}

function New-TestPfxBase64(
    [bool]$CertificateAuthority,
    [bool]$IncludeCodeSigningEku,
    [datetime]$NotBefore,
    [datetime]$NotAfter,
    [string]$Password,
    [bool]$IncludeBasicConstraints = $true,
    [bool]$IncludeDigitalSignatureUsage = $true
) {
    $key = [Security.Cryptography.RSA]::Create(2048)
    $request = [Security.Cryptography.X509Certificates.CertificateRequest]::new(
        [Security.Cryptography.X509Certificates.X500DistinguishedName]::new('CN=OpenShazam Signing Contract Test'),
        $key,
        [Security.Cryptography.HashAlgorithmName]::SHA256,
        [Security.Cryptography.RSASignaturePadding]::Pkcs1
    )
    if ($IncludeBasicConstraints) {
        $request.CertificateExtensions.Add(
            [Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($CertificateAuthority, $false, 0, $true)
        )
    }
    $keyUsage = [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::None
    if ($IncludeDigitalSignatureUsage) {
        $keyUsage = [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature
    }
    if ($CertificateAuthority) {
        $keyUsage = $keyUsage -bor [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign
    }
    $request.CertificateExtensions.Add(
        [Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new($keyUsage, $true)
    )
    if ($IncludeCodeSigningEku) {
        $usages = [Security.Cryptography.OidCollection]::new()
        $null = $usages.Add([Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.3'))
        $request.CertificateExtensions.Add(
            [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($usages, $false)
        )
    }

    $certificate = $null
    try {
        $certificate = $request.CreateSelfSigned($NotBefore.ToUniversalTime(), $NotAfter.ToUniversalTime())
        return [Convert]::ToBase64String(
            $certificate.Export([Security.Cryptography.X509Certificates.X509ContentType]::Pfx, $Password)
        )
    }
    finally {
        if ($certificate) { $certificate.Dispose() }
        $key.Dispose()
    }
}

function Assert-NoTemporaryPfx {
    $leftoverPfx = @(Get-ChildItem -LiteralPath $testTempDirectory -Filter 'openshazam-signing-*.pfx' -File)
    if ($leftoverPfx.Count -ne 0) {
        throw 'Signing script left a temporary PFX behind after certificate validation failed.'
    }
}

function Assert-SigningCredentialsCleared([string]$Name) {
    foreach ($variableName in @(
        'OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64',
        'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD'
    )) {
        $processValue = [Environment]::GetEnvironmentVariable(
            $variableName,
            [EnvironmentVariableTarget]::Process
        )
        if ($null -ne $processValue) {
            throw "Signing contract case '$Name' left $variableName in the process environment."
        }
        if (Test-Path -LiteralPath "Env:$variableName") {
            throw "Signing contract case '$Name' left $variableName in the PowerShell environment provider."
        }
    }
}

try {
    $env:RUNNER_TEMP = $testTempDirectory
    Remove-Item -LiteralPath Env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath Env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD -ErrorAction SilentlyContinue
    Invoke-SigningFailureCheck 'missing certificate' @{ RequireSignature = $true; ValidateOnly = $true } 'OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 is not configured' ''

    $probe = 'openshazam-test-' + [guid]::NewGuid().ToString('N')
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'password without certificate' @{ RequireSignature = $true; ValidateOnly = $true } 'OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD is configured without OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64' $probe

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = 'MA=='
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'malformed PFX' @{ RequireSignature = $true; ValidateOnly = $true } '' $probe
    Assert-NoTemporaryPfx

    $now = [DateTime]::UtcNow
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddMinutes(-5) $now.AddDays(1) $probe
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningSuccessCheck 'valid code-signing certificate' @{ RequireSignature = $true; ValidateOnly = $true } 'Authenticode certificate is valid for code signing.' $probe
    Assert-NoTemporaryPfx

    $blockedTempRoot = Join-Path $testTempDirectory 'not-a-directory'
    [IO.File]::WriteAllText($blockedTempRoot, 'block nested PFX creation')
    $env:RUNNER_TEMP = $blockedTempRoot
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddMinutes(-5) $now.AddDays(1) $probe
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'temporary PFX write failure' @{ RequireSignature = $true; ValidateOnly = $true } '' $probe
    Remove-Item -LiteralPath $blockedTempRoot -Force
    $env:RUNNER_TEMP = $testTempDirectory
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddMinutes(-5) $now.AddDays(1) $probe
    $wrongPassword = "$probe-wrong"
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $wrongPassword
    Invoke-SigningFailureCheck 'wrong PFX password' @{ RequireSignature = $true; ValidateOnly = $true } '' $wrongPassword
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $true $true $now.AddMinutes(-5) $now.AddDays(1) $probe
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'CA certificate' @{ RequireSignature = $true; ValidateOnly = $true } 'certificate authority' $probe
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $false $now.AddMinutes(-5) $now.AddDays(1) $probe
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'certificate without code-signing EKU' @{ RequireSignature = $true; ValidateOnly = $true } 'does not include the Code Signing enhanced key usage' $probe
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddDays(-3) $now.AddDays(-1) $probe
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'expired certificate' @{ RequireSignature = $true; ValidateOnly = $true } 'outside its validity period' $probe
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddMinutes(-5) $now.AddDays(1) $probe $false
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'certificate without Basic Constraints' @{ RequireSignature = $true; ValidateOnly = $true } 'must include a Basic Constraints extension' $probe
    Assert-NoTemporaryPfx

    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 = New-TestPfxBase64 $false $true $now.AddMinutes(-5) $now.AddDays(1) $probe $true $false
    $env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD = $probe
    Invoke-SigningFailureCheck 'certificate without digital-signature usage' @{ RequireSignature = $true; ValidateOnly = $true } 'must permit digital signatures' $probe
    Assert-NoTemporaryPfx

    Remove-Item -LiteralPath Env:OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64 -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath Env:OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD -ErrorAction SilentlyContinue
    & $signingScript *>&1 | Out-Null
}
finally {
    foreach ($name in $environmentNames) {
        if ($savedEnvironment[$name].Exists) {
            Set-Item -LiteralPath "Env:$name" -Value $savedEnvironment[$name].Value
        }
        else {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
    }
    $remaining = Get-ChildItem -LiteralPath $testTempDirectory -Force
    if ($remaining.Count -eq 0) {
        Remove-Item -LiteralPath $testTempDirectory
    }
    else {
        throw 'Signing test left unexpected files in its isolated temporary folder.'
    }
}

Write-Host 'Windows Authenticode fail-closed and temporary-PFX cleanup contract passed.'
