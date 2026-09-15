$ErrorActionPreference = 'Stop'
$monitorScript = Join-Path $PSScriptRoot 'server.cjs'
& node $monitorScript
