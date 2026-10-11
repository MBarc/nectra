# Re-renders the README images from docs/brand/src with headless Edge.
# Usage: pwsh docs/brand/render.ps1   (set $env:BROWSER_EXE to use Chrome instead)
$browser = if ($env:BROWSER_EXE) { $env:BROWSER_EXE } else { "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" }
$dir = $PSScriptRoot
$src = "file:///" + ($dir -replace '\\', '/') + "/src/"
$shots = @(
    @("banner.html", 1600, 420, "banner.png", ""),
    @("banner.html", 1600, 420, "banner-dark.png", "#dark"),
    @("how-it-works.html", 1600, 640, "how-it-works.png", ""),
    @("how-it-works.html", 1600, 640, "how-it-works-dark.png", "#dark"),
    @("social.html", 1280, 640, "social-preview.png", "#dark")
)
foreach ($s in $shots) {
    & $browser --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 `
        --virtual-time-budget=4000 "--window-size=$($s[1]),$($s[2])" `
        "--screenshot=$dir\$($s[3])" "$src$($s[0])$($s[4])" 2>$null | Out-Null
    Write-Output $s[3]
}
