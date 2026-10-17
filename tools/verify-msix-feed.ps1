param(
    [Parameter(Mandatory = $true)]
    [string]$AppInstallerUri,
    [Parameter(Mandatory = $true)]
    [string]$PackageUri
)

$ErrorActionPreference = 'Stop'

function ConvertTo-HttpsUri([string]$value, [string]$name) {
    try { $uri = [Uri]$value } catch { throw "$name is not a valid absolute URL: $value" }
    if (-not $uri.IsAbsoluteUri -or $uri.Scheme -ne 'https') {
        throw "$name must use HTTPS: $value"
    }
    return $uri
}

function Invoke-FollowingHttpsRedirects([System.Net.Http.HttpClient]$client, [string]$method, [Uri]$startUri, [switch]$RangeOneByte) {
    $current = $startUri
    for ($redirectCount = 0; $redirectCount -le 10; $redirectCount++) {
        $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($method), $current)
        if ($RangeOneByte) {
            $request.Headers.Range = [System.Net.Http.Headers.RangeHeaderValue]::new(0, 0)
        }
        $response = $client.SendAsync(
            $request,
            [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
        ).GetAwaiter().GetResult()
        $request.Dispose()

        if ([int]$response.StatusCode -in @(301, 302, 303, 307, 308)) {
            $location = $response.Headers.Location
            $response.Dispose()
            if (-not $location) { throw "Redirect from $current had no Location header." }
            $next = if ($location.IsAbsoluteUri) { $location } else { [Uri]::new($current, $location) }
            if ($next.Scheme -ne 'https') { throw "Redirect downgraded from HTTPS: $next" }
            $current = $next
            continue
        }

        if ($redirectCount -eq 10) {
            $response.Dispose()
            throw "Too many HTTPS redirects from $startUri"
        }
        return [pscustomobject]@{
            Response = $response
            FinalUri = $current
            RedirectCount = $redirectCount
        }
    }
    throw "Could not resolve HTTPS endpoint: $startUri"
}

function Assert-SuccessAndLength($result, [string]$label) {
    $response = $result.Response
    if (-not $response.IsSuccessStatusCode) {
        $status = [int]$response.StatusCode
        $response.Dispose()
        throw "$label returned HTTP $status after redirects."
    }
    $length = $response.Content.Headers.ContentLength
    if ($null -eq $length -or $length -le 0) {
        $response.Dispose()
        throw "$label response is missing a positive Content-Length header."
    }
    return [long]$length
}

function Assert-ContentType($response, [string]$expected, [string]$label) {
    $actual = $response.Content.Headers.ContentType.MediaType
    if ($actual -cne $expected) {
        throw "$label Content-Type must be '$expected'; received '$actual'."
    }
}

$appInstaller = ConvertTo-HttpsUri $AppInstallerUri 'AppInstallerUri'
$package = ConvertTo-HttpsUri $PackageUri 'PackageUri'
if (-not $appInstaller.AbsolutePath.EndsWith('.appinstaller', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'AppInstallerUri must have a direct .appinstaller path.'
}
if (-not $package.AbsolutePath.EndsWith('.msix', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'PackageUri must identify an .msix package.'
}

$handler = [System.Net.Http.HttpClientHandler]::new()
$handler.AllowAutoRedirect = $false
$handler.CheckCertificateRevocationList = $true
$client = [System.Net.Http.HttpClient]::new($handler)
$client.Timeout = [TimeSpan]::FromSeconds(45)
try {
    $appHead = Invoke-FollowingHttpsRedirects $client 'HEAD' $appInstaller
    $appHeadLength = Assert-SuccessAndLength $appHead 'App Installer HEAD'
    Assert-ContentType $appHead.Response 'application/appinstaller' 'App Installer HEAD'
    $appHeadFinal = $appHead.FinalUri
    $appHeadRedirects = $appHead.RedirectCount
    $appHead.Response.Dispose()

    $appGet = Invoke-FollowingHttpsRedirects $client 'GET' $appInstaller
    $appGetLength = Assert-SuccessAndLength $appGet 'App Installer GET'
    Assert-ContentType $appGet.Response 'application/appinstaller' 'App Installer GET'
    if ($appGetLength -ne $appHeadLength) { throw 'App Installer GET and HEAD Content-Length values differ.' }
    if ($appGetLength -gt 1MB) { throw 'App Installer manifest exceeded the 1 MiB validation limit.' }
    $appBytes = $appGet.Response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    if ($appBytes.LongLength -ne $appGetLength) { throw 'App Installer GET body length does not match Content-Length.' }
    $appGetFinal = $appGet.FinalUri
    $appGetRedirects = $appGet.RedirectCount
    $appGet.Response.Dispose()

    [xml]$appXml = [Text.Encoding]::UTF8.GetString($appBytes)
    $root = $appXml.DocumentElement
    if ($root.LocalName -ne 'AppInstaller') { throw 'The downloaded manifest root element is not AppInstaller.' }
    if ([Uri]$root.GetAttribute('Uri') -ne $appInstaller) { throw 'The AppInstaller Uri attribute does not match the configured stable URI.' }
    $mainPackage = $appXml.SelectSingleNode("//*[local-name()='MainPackage']")
    if (-not $mainPackage -or [Uri]$mainPackage.GetAttribute('Uri') -ne $package) {
        throw 'The AppInstaller MainPackage Uri does not match PackageUri.'
    }

    $packageHead = Invoke-FollowingHttpsRedirects $client 'HEAD' $package
    $packageLength = Assert-SuccessAndLength $packageHead 'MSIX HEAD'
    Assert-ContentType $packageHead.Response 'application/msix' 'MSIX HEAD'
    $packageHeadFinal = $packageHead.FinalUri
    $packageHeadRedirects = $packageHead.RedirectCount
    $packageHead.Response.Dispose()

    $packageRange = Invoke-FollowingHttpsRedirects $client 'GET' $package -RangeOneByte
    Assert-ContentType $packageRange.Response 'application/msix' 'MSIX range GET'
    if ([int]$packageRange.Response.StatusCode -ne 206) { throw 'MSIX endpoint did not honor the byte-range request with HTTP 206.' }
    $contentRange = $packageRange.Response.Content.Headers.ContentRange
    if (-not $contentRange -or $contentRange.Unit -ne 'bytes' -or $contentRange.From -ne 0 -or $contentRange.To -ne 0 -or $contentRange.Length -ne $packageLength) {
        throw 'MSIX Content-Range does not match the HEAD Content-Length.'
    }
    if ($packageRange.Response.Content.Headers.ContentLength -ne 1) { throw 'MSIX range response must contain exactly one byte.' }
    $rangeBody = $packageRange.Response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult()
    if ($rangeBody.Length -ne 1) { throw 'MSIX range response body was not one byte.' }
    $packageRangeFinal = $packageRange.FinalUri
    $packageRangeRedirects = $packageRange.RedirectCount
    $packageRange.Response.Dispose()

    [pscustomobject]@{
        AppInstallerFinalUri = $appGetFinal.AbsoluteUri
        AppInstallerRedirects = $appGetRedirects
        AppInstallerContentLength = $appGetLength
        PackageFinalUri = $packageRangeFinal.AbsoluteUri
        PackageRedirects = $packageRangeRedirects
        PackageContentLength = $packageLength
        ByteRange = '0-0/206 verified'
        TLS = 'HTTPS and certificate revocation validation enabled'
    } | Format-List
}
finally {
    $client.Dispose()
    $handler.Dispose()
}
