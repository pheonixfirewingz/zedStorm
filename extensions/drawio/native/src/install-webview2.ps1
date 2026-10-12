param(
    [Parameter(Mandatory=$true)][string]$Destination,
    [Parameter(Mandatory=$true)][ValidateSet('x64', 'arm64', 'x86')][string]$Architecture
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$pointer = Join-Path $Destination 'runtime-path.txt'
if (Test-Path $pointer) {
    $runtime = (Get-Content -Raw $pointer).Trim()
    if (Test-Path (Join-Path $runtime 'msedgewebview2.exe')) {
        Write-Output $runtime
        exit 0
    }
}
$page = (Invoke-WebRequest -UseBasicParsing 'https://developer.microsoft.com/en-us/microsoft-edge/webview2/').Content
$page = $page.Replace('\u002F', '/')
$pattern = 'https://msedge\.sf\.dl\.delivery\.mp\.microsoft\.com/filestreamingservice/files/[a-f0-9-]+/Microsoft\.WebView2\.FixedVersionRuntime\.(?<version>\d+\.\d+\.\d+\.\d+)\.' + $Architecture + '\.cab'
$release = [regex]::Matches($page, $pattern) | Sort-Object { [version]$_.Groups['version'].Value } -Descending | Select-Object -First 1
if ($null -eq $release) { throw 'Microsoft did not publish a matching fixed WebView2 download' }
$staging = Join-Path $Destination ('staging-' + [guid]::NewGuid())
New-Item -ItemType Directory $staging | Out-Null
try {
    $archive = Join-Path $staging 'runtime.cab'
    Invoke-WebRequest -UseBasicParsing $release.Value -OutFile $archive
    & expand.exe $archive '-F:*' $staging | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract WebView2 runtime' }
    $executable = Get-ChildItem $staging -Filter 'msedgewebview2.exe' -Recurse | Select-Object -First 1
    if ($null -eq $executable) { throw 'WebView2 executable missing from download' }
    $signature = Get-AuthenticodeSignature $executable.FullName
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw 'WebView2 Microsoft signature validation failed'
    }
    $runtime = Join-Path $Destination ('webview2-' + $release.Groups['version'].Value + '-' + $Architecture)
    if (Test-Path $runtime) { Remove-Item -Recurse -Force $runtime }
    Move-Item $executable.Directory.FullName $runtime
    & icacls.exe $runtime '/grant' '*S-1-15-2-2:(OI)(CI)(RX)' '*S-1-15-2-1:(OI)(CI)(RX)' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant WebView2 renderer access' }
    Set-Content -NoNewline -Path $pointer -Value $runtime
    Write-Output $runtime
} finally {
    Remove-Item -Recurse -Force $staging
}
