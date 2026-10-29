Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host " ZedStorm Context MCP - All Tools Demonstration" -ForegroundColor Cyan
Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host ""

$zed = "C:\Users\ai_ms\zedStorm\target\debug\zed.exe"
$project = "C:\Users\ai_ms\zedStorm"

if (-not (Test-Path $zed)) {
    Write-Host "Building zed first..." -ForegroundColor Yellow
    cargo build -p zed
}

# Start with --context-mcp-write to expose all tools (including edit, mkdir, copy, move, delete, build)
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $zed
$psi.Arguments = "--context-mcp `"$project`" --context-mcp-write"
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$p = [System.Diagnostics.Process]::Start($psi)

$script:reqId = 1
function Call-Rpc($method, $params = $null) {
    $req = @{
        jsonrpc = "2.0"
        id = $script:reqId
        method = $method
    }
    if ($null -ne $params) {
        $req["params"] = $params
    }
    $script:reqId++
    
    $json = ConvertTo-Json -InputObject $req -Depth 10 -Compress
    $p.StandardInput.WriteLine($json)
    $p.StandardInput.Flush()
    
    $rawLine = $p.StandardOutput.ReadLine()
    return ($rawLine | ConvertFrom-Json)
}

function Notify-Rpc($method, $params = $null) {
    $req = @{
        jsonrpc = "2.0"
        method = $method
    }
    if ($null -ne $params) {
        $req["params"] = $params
    }
    $json = ConvertTo-Json -InputObject $req -Depth 10 -Compress
    $p.StandardInput.WriteLine($json)
    $p.StandardInput.Flush()
}

function Execute-Tool($toolName, $arguments, $title) {
    Write-Host "-----------------------------------------------------------------" -ForegroundColor DarkYellow
    Write-Host (">>> TOOL: {0} ({1})" -f $toolName, $title) -ForegroundColor Green
    Write-Host ("Input: " + (ConvertTo-Json $arguments -Compress)) -ForegroundColor DarkGray
    
    $resp = Call-Rpc "tools/call" @{
        name = $toolName
        arguments = $arguments
    }
    
    $text = ""
    if ($null -ne $resp.result.content -and $resp.result.content.Count -gt 0) {
        $text = $resp.result.content[0].text
    }
    
    if ($resp.result.isError) {
        Write-Host "Response [NOTICE / ERROR]:" -ForegroundColor Red
        Write-Host $text -ForegroundColor Red
    } else {
        Write-Host "Response [SUCCESS]:" -ForegroundColor Yellow
        Write-Host $text -ForegroundColor White
    }
    
    return [PSCustomObject]@{
        IsError = $resp.result.isError
        Text = $text
    }
}

function Get-Revision($text) {
    if ($text -match 'revision=([0-9a-f]+)') {
        return $matches[1]
    }
    return ""
}

# 1. INITIALIZE
Write-Host ">>> [1] INITIALIZE" -ForegroundColor Green
$initResp = Call-Rpc "initialize" @{
    protocolVersion = "2025-06-18"
    capabilities = @{}
    clientInfo = @{ name = "terminal-all-tools"; version = "1.0" }
}
Notify-Rpc "notifications/initialized"
Write-Host "Server initialized: $($initResp.result.serverInfo.name) v$($initResp.result.serverInfo.version)" -ForegroundColor DarkGray
Write-Host ""

# 2. TOOLS/LIST
Write-Host ">>> [2] TOOLS/LIST" -ForegroundColor Green
$toolsResp = Call-Rpc "tools/list" @{}
$tools = $toolsResp.result.tools
Write-Host ("Total tools exposed: {0}" -f $tools.Count) -ForegroundColor Yellow
foreach ($t in $tools) {
    $writeTag = if ($t.annotations.readOnlyHint) { "[RO]" } else { "[WR]" }
    $color = if ($t.annotations.readOnlyHint) { "DarkCyan" } else { "Magenta" }
    Write-Host ("  {0} {1,-18} - {2}" -f $writeTag, $t.name, $t.description.Split('.')[0]) -ForegroundColor $color
}
Write-Host ""

# 3. FILESYSTEM READ TOOLS
Write-Host "=================== [ 1. FILESYSTEM READ TOOLS ] ===================" -ForegroundColor Cyan

# list
$null = Execute-Tool "list" @{ path = "crates/context_mcp"; depth = 1 } "List directory entries"

# stat
$null = Execute-Tool "stat" @{ path = "crates/context_mcp/Cargo.toml" } "Stat file metadata"

# files
$null = Execute-Tool "files" @{ path = "crates/context_mcp"; query = "git" } "Find files by substring"

# search
$null = Execute-Tool "search" @{ path = "crates/context_mcp"; query = "repositories" } "Search text in files"

# read
$readRes = Execute-Tool "read" @{ path = "crates/context_mcp/Cargo.toml"; start_line = 1; end_line = 12 } "Read lines 1-12 of Cargo.toml"

Write-Host ""
Write-Host "=================== [ 2. GIT EXPLORATION TOOLS ] ===================" -ForegroundColor Cyan

# git_repos
$null = Execute-Tool "git_repos" @{ path = "." } "Discover git repositories"

# git_repos nested
$null = Execute-Tool "git_repos" @{ path = "."; include_nested = $true; depth = 5 } "Discover repositories with nested"

# git_read status
$null = Execute-Tool "git_read" @{ path = "."; query = "status" } "Git status"

# git_read remotes
$null = Execute-Tool "git_read" @{ path = "."; query = "remotes" } "Git remotes"

# git_read diff
$null = Execute-Tool "git_read" @{ path = "."; query = "diff" } "Git diff --numstat"

# git_read log
$null = Execute-Tool "git_read" @{ path = "."; query = "log"; max_commits = 5 } "Git log (last 5 commits)"

Write-Host ""
Write-Host "================== [ 3. FILESYSTEM MUTATION TOOLS ] =================" -ForegroundColor Cyan

$demoDir = "target/mcp_live_demo"
$demoFile = "$demoDir/demo.txt"
$copyFile = "$demoDir/demo_copy.txt"
$movedFile = "$demoDir/demo_moved.txt"

# mkdir
$null = Execute-Tool "mkdir" @{ path = $demoDir; parents = $true } "Create temporary demo directory"

# edit (create new file)
$createRes = Execute-Tool "edit" @{
    path = $demoFile
    revision = "new"
    old_text = ""
    new_text = "Hello from Context MCP!`nLine 2: Demonstration of edit tool.`nLine 3: End of initial text.`n"
} "Create a new text file"

# read newly created file to get revision
$readRes = Execute-Tool "read" @{ path = $demoFile } "Read created file to get current revision"
$rev = Get-Revision $readRes.Text
Write-Host ("Extracted revision: {0}" -f $rev) -ForegroundColor DarkGray

# edit (modify existing file using revision)
$editRes = Execute-Tool "edit" @{
    path = $demoFile
    revision = $rev
    old_text = "Line 2: Demonstration of edit tool."
    new_text = "Line 2: MODIFIED content successfully via MCP edit tool!"
} "Replace text using read revision"
$rev2 = Get-Revision $editRes.Text
Write-Host ("New revision after edit: {0}" -f $rev2) -ForegroundColor DarkGray

# read again to verify modification
$null = Execute-Tool "read" @{ path = $demoFile } "Verify modified text"

# copy
$copyRes = Execute-Tool "copy" @{
    source = $demoFile
    destination = $copyFile
    revision = $rev2
} "Copy file using revision"
$copyRev = Get-Revision $copyRes.Text

# read copy
$null = Execute-Tool "read" @{ path = $copyFile } "Read copy to verify content"

# move
$moveRes = Execute-Tool "move" @{
    source = $copyFile
    destination = $movedFile
    revision = $copyRev
} "Move file (copy then delete source)"
$movedRev = Get-Revision $moveRes.Text

# read moved file
$null = Execute-Tool "read" @{ path = $movedFile } "Read moved file to verify content"

# delete moved file
$null = Execute-Tool "delete" @{
    path = $movedFile
    revision = $movedRev
} "Delete moved file"

# delete original demo file
$null = Execute-Tool "delete" @{
    path = $demoFile
    revision = $rev2
} "Delete demo file"

Write-Host ""
Write-Host "=================== [ 4. SEMANTIC / EDITOR TOOLS ] ===================" -ForegroundColor Cyan

# semantic_status
$null = Execute-Tool "semantic_status" @{} "Check language server status (requires active ZedStorm editor window)"

$p.StandardInput.Close()
$p.WaitForExit(3000) | Out-Null

Write-Host ""
Write-Host "=================================================================" -ForegroundColor Cyan
Write-Host " All tools executed successfully! Terminal kept open." -ForegroundColor Cyan
Write-Host "=================================================================" -ForegroundColor Cyan
