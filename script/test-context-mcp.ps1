param([Parameter(Mandatory = $true)][string]$Executable)

$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This smoke test requires native Windows' }
$executablePath = (Resolve-Path $Executable).Path
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) "ZedStorm MCP é $([guid]::NewGuid())"
$project = Join-Path $fixture 'project with spaces'
New-Item -ItemType Directory -Path $project -Force | Out-Null
$process = $null

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-Mcp([int]$Id, [string]$Method, [hashtable]$Parameters, [switch]$ExpectError) {
    $request = @{ jsonrpc = '2.0'; id = $Id; method = $Method; params = $Parameters }
    $process.StandardInput.WriteLine(($request | ConvertTo-Json -Depth 20 -Compress))
    $process.StandardInput.Flush()
    $pending = $process.StandardOutput.ReadLineAsync()
    $timeout = if ($Method -eq 'tools/call' -and $Parameters.name -eq 'build') { 650000 } else { 10000 }
    if (-not $pending.Wait($timeout)) { throw "Timed out waiting for MCP response to $Method" }
    if ($null -eq $pending.Result) { throw 'MCP process closed stdout unexpectedly' }
    $response = $pending.Result | ConvertFrom-Json -AsHashtable
    Assert-Condition ($response.id -eq $Id) 'Unexpected MCP response id'
    Assert-Condition ($response.jsonrpc -eq '2.0') 'Invalid MCP response'
    if ($response.ContainsKey('error')) { throw ($response.error | ConvertTo-Json -Compress) }
    if ($ExpectError) {
        Assert-Condition ($response.result.isError -eq $true) 'Expected tool rejection'
    } elseif ($response.result.isError) {
        throw $response.result.content[0].text
    }
    return $response.result
}

function Invoke-Tool([int]$Id, [string]$Name, [hashtable]$Arguments, [switch]$ExpectError) {
    return Invoke-Mcp -Id $Id -Method 'tools/call' -Parameters @{ name = $Name; arguments = $Arguments } -ExpectError:$ExpectError
}

try {
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $executablePath
    $start.WorkingDirectory = $fixture
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $encoding = [System.Text.UTF8Encoding]::new($false)
    $start.StandardInputEncoding = $encoding
    $start.StandardOutputEncoding = $encoding
    $start.StandardErrorEncoding = $encoding
    $start.ArgumentList.Add('--context-mcp')
    $start.ArgumentList.Add($project)
    $start.ArgumentList.Add('--context-mcp-write')
    $process = [System.Diagnostics.Process]::Start($start)
    $diagnostics = $process.StandardError.ReadToEndAsync()
    Invoke-Mcp 1 'initialize' @{ protocolVersion = '2025-06-18'; capabilities = @{}; clientInfo = @{ name = 'windows-smoke'; version = '1' } } | Out-Null
    $process.StandardInput.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}')
    $tools = Invoke-Mcp 2 'tools/list' @{}
    foreach ($name in @('list', 'stat', 'files', 'search', 'read', 'edit', 'delete', 'mkdir', 'copy', 'move', 'build')) {
        Assert-Condition ($tools.tools.name -contains $name) "Missing tool: $name"
    }
    Invoke-Tool 3 'mkdir' @{ path = 'nested'; parents = $false } | Out-Null
    Invoke-Tool 4 'edit' @{ path = 'nested/é file.txt'; revision = 'new'; old_text = ''; new_text = "first`r`nsecond`r`n" } | Out-Null
    $read = Invoke-Tool 5 'read' @{ path = 'nested\é file.txt'; max_bytes = 256 }
    Assert-Condition ($read.content[0].text -match 'revision=([a-f0-9]{64})') 'Missing revision'
    $revision = $Matches[1]
    Invoke-Tool 6 'edit' @{ path = 'nested/é file.txt'; revision = $revision; old_text = 'first'; new_text = 'updated' } | Out-Null
    Assert-Condition ([System.IO.File]::ReadAllText((Join-Path $project 'nested/é file.txt')) -ceq "updated`r`nsecond`r`n") 'Edit changed CRLF line endings'
    $read = Invoke-Tool 7 'read' @{ path = 'nested/é file.txt' }
    Assert-Condition ($read.content[0].text -match 'revision=([a-f0-9]{64})') 'Missing edited revision'
    $revision = $Matches[1]
    $listing = Invoke-Tool 8 'list' @{ depth = 2 }
    Assert-Condition ($listing.content[0].text.Contains('nested/é file.txt')) 'Listing paths are not portable'
    $metadata = Invoke-Tool 9 'stat' @{ path = 'nested/é file.txt' }
    Assert-Condition ($metadata.content[0].text.Contains('type=file')) 'stat failed'
    $files = Invoke-Tool 10 'files' @{ query = 'nested/' }
    Assert-Condition ($files.content[0].text.Contains('nested/é file.txt')) 'File discovery paths are not portable'
    $search = Invoke-Tool 11 'search' @{ query = 'updated' }
    Assert-Condition ($search.content[0].text.Contains('nested/é file.txt:1:0')) 'Search paths are not portable'
    $source = Join-Path $project 'nested/é file.txt'
    [System.IO.File]::SetAttributes($source, [System.IO.FileAttributes]::ReadOnly)
    Invoke-Tool 12 'copy' @{ source = 'nested/é file.txt'; destination = 'copy.txt'; revision = $revision } | Out-Null
    $copy = Join-Path $project 'copy.txt'
    Assert-Condition (([System.IO.File]::GetAttributes($copy) -band [System.IO.FileAttributes]::ReadOnly) -ne 0) 'Copy lost read-only attribute'
    [System.IO.File]::SetAttributes($source, [System.IO.FileAttributes]::Normal)
    [System.IO.File]::SetAttributes($copy, [System.IO.FileAttributes]::Normal)
    Invoke-Tool 13 'move' @{ source = 'copy.txt'; destination = 'moved.txt'; revision = $revision } | Out-Null
    Assert-Condition (-not (Test-Path $copy)) 'Move retained source'
    Invoke-Tool 14 'delete' @{ path = 'moved.txt'; revision = $revision } | Out-Null
    Assert-Condition (-not (Test-Path (Join-Path $project 'moved.txt'))) 'Delete retained file'
    Invoke-Tool 15 'mkdir' @{ path = 'NUL.txt' } -ExpectError | Out-Null
    Invoke-Tool 16 'stat' @{ path = 'nested/é file.txt:stream' } -ExpectError | Out-Null
    $outside = Join-Path $fixture 'outside'
    New-Item -ItemType Directory -Path $outside | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $project 'junction') -Target $outside | Out-Null
    Invoke-Tool 17 'mkdir' @{ path = 'junction/new' } -ExpectError | Out-Null
    $buildProject = Join-Path $project 'build project'
    New-Item -ItemType Directory -Path $buildProject | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $buildProject 'Cargo.toml'), "[package]`nname = 'mcp_smoke'`nversion = '0.1.0'`nedition = '2024'`n[lib]`npath = 'source.rs'`n", $encoding)
    [System.IO.File]::WriteAllText((Join-Path $buildProject 'source.rs'), 'pub fn value() -> u32 { 1 }', $encoding)
    $built = Invoke-Tool 18 'build' @{ path = 'build project' }
    Assert-Condition ($built.content[0].text -ceq 'built') 'Successful build included unnecessary output'
    [System.IO.File]::WriteAllText((Join-Path $buildProject 'source.rs'), 'pub fn value() -> u32 { missing_symbol }', $encoding)
    $failed = Invoke-Tool 19 'build' @{ path = 'build project' } -ExpectError
    Assert-Condition ($failed.content[0].text.Contains('missing_symbol')) 'Failed build omitted compiler error'
    Assert-Condition (-not $failed.content[0].text.Contains('compiler-artifact')) 'Build leaked compiler logs'
    $process.StandardInput.Close()
    Assert-Condition ($process.WaitForExit(10000)) 'MCP did not exit at EOF'
    Assert-Condition ($process.ExitCode -eq 0) "MCP exited with $($process.ExitCode): $($diagnostics.Result)"
    Write-Output 'Native Windows MCP smoke passed: all eleven tools, build results, UTF-8 paths, CRLF, read-only copies, path restrictions, junctions, and stdio lifecycle'
} finally {
    if ($null -ne $process) {
        if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }
        $process.Dispose()
    }
    if (Test-Path $fixture) { Remove-Item -LiteralPath $fixture -Recurse -Force }
}
