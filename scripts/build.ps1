param([switch]$SkipChecks)
$ErrorActionPreference = 'Stop'
$project = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargoExe = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path -LiteralPath $cargoExe)) { throw 'Install stable Rust before building Nectra.' }
function Invoke-Cargo {
    & $cargoExe @args
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }
}
Push-Location $project
try {
    if (-not $SkipChecks) {
        Invoke-Cargo fmt --all --check
        Invoke-Cargo test --all-targets --locked
        Invoke-Cargo clippy --all-targets --locked -- -D warnings
        Invoke-Cargo run --locked -- eval eval/tasks.json
    }
    Invoke-Cargo build --release --locked
    $binary = Join-Path $project 'target\release\nectra.exe'
    if (-not (Test-Path -LiteralPath $binary)) { throw 'Expected Windows release binary is missing.' }
    $dist = Join-Path $project 'dist'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $archive = Join-Path $dist 'nectra-windows-x64.zip'
    if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') { throw 'This local packaging script supports Windows x64 only.' }
    Compress-Archive -LiteralPath $binary,(Join-Path $project 'README.md'),(Join-Path $project 'LICENSE'),(Join-Path $project 'docs'),(Join-Path $project 'scripts\uninstall-nectra.ps1') -DestinationPath $archive -Force
    $digest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText("$archive.sha256", "$digest  nectra-windows-x64.zip`n")
    Write-Output "Built: $binary"
    Write-Output "Packaged: $archive"
} finally { Pop-Location }
