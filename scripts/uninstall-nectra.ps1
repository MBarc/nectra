[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [string]$InstallDirectory
)
$ErrorActionPreference = 'Stop'
if (-not $PSBoundParameters.ContainsKey('InstallDirectory')) {
    if ((Test-Path -LiteralPath (Join-Path $PSScriptRoot 'install.ps1')) -and (Test-Path -LiteralPath (Join-Path $PSScriptRoot '..\Cargo.toml'))) {
        $InstallDirectory = Join-Path $env:USERPROFILE '.nectra\bin'
    } else { $InstallDirectory = $PSScriptRoot }
}
if ([string]::IsNullOrWhiteSpace($InstallDirectory)) { throw 'InstallDirectory cannot be empty.' }
$target = [System.IO.Path]::GetFullPath($InstallDirectory).TrimEnd('\', '/')
if (-not $target -or $target -eq [System.IO.Path]::GetPathRoot($target).TrimEnd('\', '/')) {
    throw 'Specify a Nectra installation directory, not a drive root.'
}

# Remove only known installed files. Never recurse into the installation directory.
$binary = Join-Path $target 'nectra.exe'
if ((Test-Path -LiteralPath $binary -PathType Leaf) -and $PSCmdlet.ShouldProcess($binary, 'Remove Nectra executable')) {
    try { Remove-Item -LiteralPath $binary -ErrorAction Stop }
    catch { throw "Could not remove Nectra. Close any running Nectra sessions and try again. $($_.Exception.Message)" }
}

# Match a complete PATH entry, including equivalent casing, quotes and trailing slashes.
$userPath = [string][Environment]::GetEnvironmentVariable('Path', 'User')
$remaining = @($userPath -split ';' | Where-Object {
    $entry = [Environment]::ExpandEnvironmentVariables($_.Trim().Trim('"')).TrimEnd('\', '/')
    $entry -ine $target
})
$newPath = $remaining -join ';'
if ($newPath -cne $userPath -and $PSCmdlet.ShouldProcess('User PATH', "Remove the entry for $target")) {
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
}
$helper = Join-Path $target 'uninstall-nectra.ps1'
if ((Test-Path -LiteralPath $helper -PathType Leaf) -and $PSCmdlet.ShouldProcess($helper, 'Remove Nectra uninstaller')) {
    Remove-Item -LiteralPath $helper -ErrorAction Stop
}
if ($WhatIfPreference) { Write-Output 'Preview only. No files or PATH entries were changed.' }
else { Write-Output 'Nectra uninstalled. Saved sessions and configuration were kept. Open a new terminal to refresh PATH.' }
