$ErrorActionPreference = "Stop"
$repo = $env:NECTRA_REPO
if (-not $repo -or $repo -notmatch '^[\w.-]+/[\w.-]+$') { throw "Set NECTRA_REPO to the trusted GitHub owner/repository before installing." }
$version = if ($env:NECTRA_VERSION) { $env:NECTRA_VERSION } else { "latest" }
if ($version -notmatch '^[\w.-]+$') { throw "Invalid NECTRA_VERSION." }
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne "X64") { throw "Prebuilt Windows releases currently support x64 only. Build from source on other architectures." }
$name = "nectra-x86_64-pc-windows-msvc.zip"
$base = "https://github.com/$repo/releases"
$base = if ($version -eq "latest") { "$base/latest/download" } else { "$base/download/$version" }
$dest = Join-Path $HOME ".nectra\bin"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("nectra-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $stage | Out-Null
$archive = Join-Path $stage $name
$checksum = "$archive.sha256"
try {
    Invoke-WebRequest -Uri "$base/$name" -OutFile $archive
    Invoke-WebRequest -Uri "$base/$name.sha256" -OutFile $checksum
    $expected = ((Get-Content -LiteralPath $checksum -Raw).Trim() -split '\s+')[0]
    if ($expected -notmatch '^[a-fA-F0-9]{64}$' -or (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw "Archive SHA-256 verification failed." }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $entry = $zip.GetEntry("nectra.exe")
        if (-not $entry) { throw "Release archive does not contain nectra.exe." }
        $stagedExe = Join-Path $stage "nectra.exe"
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $stagedExe)
        $uninstaller = $zip.GetEntry("uninstall-nectra.ps1")
        if (-not $uninstaller) { throw "Release archive does not contain uninstall-nectra.ps1." }
        $stagedUninstaller = Join-Path $stage "uninstall-nectra.ps1"
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($uninstaller, $stagedUninstaller)
    } finally { $zip.Dispose() }
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item -LiteralPath $stagedExe -Destination (Join-Path $dest "nectra.exe") -Force
    Copy-Item -LiteralPath $stagedUninstaller -Destination (Join-Path $dest "uninstall-nectra.ps1") -Force
    $userPath = [string][Environment]::GetEnvironmentVariable("Path", "User")
    if (@($userPath -split ';') -notcontains $dest) {
        [Environment]::SetEnvironmentVariable("Path", (($userPath.TrimEnd(';') + ";" + $dest).TrimStart(';')), "User")
    }
    Write-Host "Installed Nectra to $dest. Selecting providers and checking CLI dependencies..."
    $setupArgs = @("setup", "--if-needed")
    if ($env:NECTRA_PROVIDERS) { $setupArgs += @("--providers", $env:NECTRA_PROVIDERS) }
    & (Join-Path $dest "nectra.exe") @setupArgs
    if ($LASTEXITCODE -ne 0) { throw "Nectra was installed, but provider setup is incomplete. Fix the reported CLI dependencies and run nectra setup. For unattended installs, set NECTRA_PROVIDERS to comma-separated IDs or none." }
    Write-Host "Setup complete. Open a new terminal and run: nectra"
    Write-Host "To uninstall from PowerShell, run: uninstall-nectra"
} finally {
    foreach ($file in @($archive, $checksum, (Join-Path $stage "nectra.exe"), (Join-Path $stage "uninstall-nectra.ps1"))) {
        if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file }
    }
    Remove-Item -LiteralPath $stage
}
