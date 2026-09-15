$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([System.IO.Path]::GetTempPath()) ('nectra-uninstall-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture | Out-Null
$script = Join-Path $PSScriptRoot 'uninstall-nectra.ps1'
$helper = Join-Path $fixture 'uninstall-nectra.ps1'
Copy-Item -LiteralPath $script -Destination $helper
foreach ($name in @('nectra.exe', 'other-tool.exe', 'config.toml', 'saved-session.jsonl')) {
    [System.IO.File]::WriteAllText((Join-Path $fixture $name), 'test fixture')
}
$pathBefore = [Environment]::GetEnvironmentVariable('Path', 'User')
& $helper -WhatIf
foreach ($name in @('nectra.exe', 'uninstall-nectra.ps1', 'other-tool.exe', 'config.toml', 'saved-session.jsonl')) {
    if (-not (Test-Path -LiteralPath (Join-Path $fixture $name))) { throw "Preview removed $name" }
}
# Execute the installed helper itself, verifying it can also remove its own file.
& $helper
foreach ($name in @('nectra.exe', 'uninstall-nectra.ps1')) {
    if (Test-Path -LiteralPath (Join-Path $fixture $name)) { throw "Uninstall left $name" }
}
foreach ($name in @('other-tool.exe', 'config.toml', 'saved-session.jsonl')) {
    if ((Get-Content -LiteralPath (Join-Path $fixture $name) -Raw) -ne 'test fixture') { throw "Uninstall changed $name" }
}
& $script -InstallDirectory $fixture
Copy-Item -LiteralPath $script -Destination $helper
& $helper
if (Test-Path -LiteralPath $helper) { throw 'A portable helper without a binary targeted the wrong directory' }
if ([Environment]::GetEnvironmentVariable('Path', 'User') -cne $pathBefore) { throw 'Uninstall changed an unrelated user PATH' }
Write-Output 'Uninstall checks passed: preview, removal, self-removal, preservation, repeat invocation, and unchanged unrelated PATH.'
