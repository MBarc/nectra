# Run adapt.py daily on this Windows PC, against the CLIs you actually use. It runs
# check.py, puts mechanical fixes on a local compat/auto-* branch, updates the
# installed models.toml (with a backup) and asks for a Laya retrain when needed.
# Live checks make one tiny turn per provider. See docs/compat.md.
#
#   powershell -File scripts\compat\schedule.ps1          # register (daily 09:00)
#   powershell -File scripts\compat\schedule.ps1 -Remove  # unregister
#
# Logs and reports go to %LOCALAPPDATA%\nectra\compat; anything that needs you shows a notification.
param([switch]$Run, [switch]$Remove, [string]$At = "09:00", [string]$Python = "", [string]$Nectra = "")
$ErrorActionPreference = "Stop"
$name = "Nectra CLI compatibility check"

if ($Remove) {
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
    "Removed '$name'."
    return
}

if (-not $Run) {
    if (-not $Python) { $Python = (Get-Command python).Source }
    if (-not $Nectra) {
        $found = Get-Command nectra -ErrorAction SilentlyContinue
        $Nectra = if ($found) { $found.Source } else { (Resolve-Path (Join-Path $PSScriptRoot "..\..\target\release\nectra.exe")).Path }
    }
    $action = New-ScheduledTaskAction -Execute "powershell.exe" `
        -Argument "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$PSCommandPath`" -Run -Python `"$Python`" -Nectra `"$Nectra`""
    $trigger = New-ScheduledTaskTrigger -Daily -At $At
    # Runs only while you're signed in: the CLIs use your logins and a console.
    $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Hours 1)
    Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    "Registered '$name' daily at $At. Logs: $env:LOCALAPPDATA\nectra\compat"
    return
}

$logs = Join-Path $env:LOCALAPPDATA "nectra\compat"
New-Item -ItemType Directory -Force $logs | Out-Null
$log = Join-Path $logs ("{0:yyyy-MM-dd}.log" -f (Get-Date))
$config = (& $Nectra config --print) -join "`n"
# check.py knows the CLI providers; Ollama has no CLI to check.
$listed = [regex]::Match($config, 'providers\s*=\s*\[([^\]]*)\]').Groups[1].Value -split '[",\s]+'
$providers = ($listed | Where-Object { $_ -in "anthropic", "openai", "google" }) -join ","
if (-not $providers) { "No provider CLIs selected; nothing to check." | Out-File $log; exit 0 }
& $Python (Join-Path $PSScriptRoot "adapt.py") --nectra $Nectra --providers $providers --live *> $log
if ($LASTEXITCODE -ne 0) {
    $issues = Select-String -Path $log -Pattern '^- \*\*' | Select-Object -First 3 | ForEach-Object { $_.Line }
    $failed = (@($issues) + "Report: $logs\report-$('{0:yyyy-MM-dd}' -f (Get-Date)).md") -join "`n"
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    $toast = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
    $text = $toast.GetElementsByTagName("text")
    $text.Item(0).AppendChild($toast.CreateTextNode("Nectra: provider CLIs need attention")) | Out-Null
    $text.Item(1).AppendChild($toast.CreateTextNode("$failed`nLog: $log")) | Out-Null
    $powershell = "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe"
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($powershell).Show(
        [Windows.UI.Notifications.ToastNotification]::new($toast))
}
exit $LASTEXITCODE
