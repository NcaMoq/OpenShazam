param(
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $env:LOCALAPPDATA 'OpenShazamReleaseSigning\ed25519-seed.base64'
}
$fullPath = [IO.Path]::GetFullPath($OutputPath)
$directory = Split-Path -Parent $fullPath
if (Test-Path -LiteralPath $fullPath) {
    throw "Refusing to overwrite an existing update signing key: $fullPath"
}

New-Item -ItemType Directory -Path $directory -Force | Out-Null
$identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
$sid = $identity.Value
& icacls.exe $directory /inheritance:r
if ($LASTEXITCODE -ne 0) { throw 'Could not remove inherited access from the update signing key folder.' }
$grant = "*$($sid):(OI)(CI)F"
& icacls.exe $directory /grant:r $grant
if ($LASTEXITCODE -ne 0) { throw 'Could not restrict the update signing key folder to the current user.' }

$seed = [byte[]]::new(32)
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
try { $random.GetBytes($seed) } finally { $random.Dispose() }
$encoded = [Convert]::ToBase64String($seed)
[Array]::Clear($seed, 0, $seed.Length)
[IO.File]::WriteAllText($fullPath, "$encoded`n", [Text.UTF8Encoding]::new($false))

Push-Location $root
try {
    $publicKey = & cargo +1.85.0 run --quiet --locked -p openshazam-update --bin openshazam-update-tool -- `
        public-key $fullPath
    if ($LASTEXITCODE -ne 0) { throw "Could not derive the Ed25519 public key (exit code $LASTEXITCODE)." }
}
finally {
    Pop-Location
}

Write-Output "Private seed saved with current-user-only ACL: $fullPath"
Write-Output "Public key (embed in TRUSTED_PUBLIC_KEY_HEX): $publicKey"
