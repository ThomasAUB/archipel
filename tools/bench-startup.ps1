<#
.SYNOPSIS
  Measures desktop app startup: process launch -> first frame (spec: < 1 s).

.DESCRIPTION
  Launches the release build several times with ARCHIPEL_STARTUP_TRACE=1 and
  reads the milestones the app prints on stderr. Times are measured from a
  timestamp taken just before the launch, using the app's own wall clock at
  `main`, so they do not include the latency of polling for a window handle
  (which adds ~0.5 s and made earlier measurements look like 1.1 s).

  The first run after a build is usually slower (antivirus scan of the new
  executable); the median is what to compare.

.EXAMPLE
  cargo build --release -p archipel-ui
  ./tools/bench-startup.ps1 -ArchipelHome C:\path\to\archipel-home -Runs 9
  pwsh ./tools/bench-startup.ps1 -ArchipelHome ~/archipel-home   # Linux / macOS
#>
param(
  [Parameter(Mandatory = $true)][string]$ArchipelHome,
  [int]$Runs = 7,
  # `$IsWindows` is unset in Windows PowerShell 5, set in PowerShell 7 (also on Linux / macOS).
  [string]$Exe = (Join-Path $PSScriptRoot ("../target/release/archipel" + $(if ($IsWindows -ne $false) { ".exe" } else { "" })))
)
$env:ARCHIPEL_HOME = $ArchipelHome
$env:ARCHIPEL_STARTUP_TRACE = "1"
$frames = @(); $milestones = @{}
foreach ($run in 1..$Runs) {
  $psi = New-Object Diagnostics.ProcessStartInfo $Exe
  $psi.UseShellExecute = $false; $psi.RedirectStandardError = $true
  $t0 = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $p = [Diagnostics.Process]::Start($psi)
  $main = $null
  while ($true) {
    $task = $p.StandardError.ReadLineAsync()
    if (-not $task.Wait(8000) -or $null -eq $task.Result) { break }
    $line = $task.Result
    if ($line -match "^startup main-wall-ms (\d+)") { $main = [int64]$Matches[1] - $t0; continue }
    if ($line -match "^startup (.+) ([\d.]+)$") {
      $name = $Matches[1]; $ms = [double]$Matches[2] + $main
      if (-not $milestones.ContainsKey($name)) { $milestones[$name] = @() }
      $milestones[$name] += $ms
      if ($name -eq "first frame") { $frames += [math]::Round($ms); break }
    }
  }
  Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 400
}
function Median($values) { $sorted = $values | Sort-Object; $sorted[[int][math]::Floor($sorted.Count / 2)] }
$milestones.GetEnumerator() | Where-Object { $_.Key -notlike "compose/*" } |
  Sort-Object { Median $_.Value } |
  ForEach-Object { "{0,8:N0} ms  {1}" -f (Median $_.Value), $_.Key }
"launch -> first frame (ms): " + ($frames -join ", ") + "   median " + (Median $frames)
