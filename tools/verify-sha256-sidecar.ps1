param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

$installer = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
if ($installer.PSIsContainer -or
    ($installer.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'The installer checksum target must be a regular file.'
}

$installerHash = (Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
$expectedSidecar = "$installerHash *$($installer.Name)`n"
$expectedBytes = [Text.UTF8Encoding]::new($false).GetBytes($expectedSidecar)
$sidecarPath = "$($installer.FullName).sha256"
$sidecar = Get-Item -LiteralPath $sidecarPath -Force -ErrorAction Stop
if ($sidecar.PSIsContainer -or
    ($sidecar.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw 'The installer checksum sidecar must be a regular file.'
}
if ($sidecar.Length -ne $expectedBytes.Length) {
    throw 'The installer SHA-256 sidecar does not exactly match the installer and file name.'
}

$actualBytes = [IO.File]::ReadAllBytes($sidecar.FullName)
if ([Convert]::ToBase64String($actualBytes) -cne [Convert]::ToBase64String($expectedBytes)) {
    throw 'The installer SHA-256 sidecar does not exactly match the installer and file name.'
}

Write-Output $installerHash
