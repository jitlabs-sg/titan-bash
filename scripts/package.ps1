param(
  [string]$IsccPath = "",
  [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Get-CargoVersion {
  $toml = Get-Content -Encoding UTF8 -Path (Join-Path $repoRoot "Cargo.toml")
  foreach ($line in $toml) {
    if ($line -match '^\s*version\s*=\s*"([^"]+)"\s*$') {
      return $Matches[1]
    }
  }
  throw "Failed to parse version from Cargo.toml"
}

$version = Get-CargoVersion

Write-Host "Building TITAN Bash v$version (release)..."
$targetDir = Join-Path $repoRoot "target\\packaging"
cargo build --release --target-dir $targetDir
if ($LASTEXITCODE -ne 0) {
  throw "cargo build failed with exit code $LASTEXITCODE"
}

$dist = Join-Path $repoRoot "dist"
New-Item -ItemType Directory -Force -Path $dist | Out-Null

$srcExe = Join-Path $targetDir "release\\titanbash.exe"
if (-not (Test-Path $srcExe)) {
  throw "Expected build output missing: $srcExe"
}

$portableName = "titanbash-$version-portable.exe"
$portableOut = Join-Path $dist $portableName
try {
  Copy-Item -Force $srcExe $portableOut -ErrorAction Stop
  Write-Host "Portable: $portableOut"
} catch {
  $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
  $fallbackName = "titanbash-$version-portable-$stamp.exe"
  $fallbackOut = Join-Path $dist $fallbackName
  Copy-Item -Force $srcExe $fallbackOut -ErrorAction Stop
  Write-Host "Portable (fallback): $fallbackOut"
  Write-Host "Note: could not overwrite $portableOut (in use). Close the running EXE to reuse the stable filename."
  if (-not $SkipInstaller) {
    throw "Cannot build installer while $portableOut is locked (running). Close titanbash and re-run."
  }
}

# Optional unversioned copy for convenience (may fail if an older copy is running)
$portableExe = Join-Path $dist "titanbash.exe"
try {
  Copy-Item -Force $srcExe $portableExe -ErrorAction Stop
} catch {
  Write-Host "Note: could not overwrite $portableExe (in use). Use the versioned portable EXE instead."
}

# Bundle BusyBox (busybox-w32) as a sidecar tool: dist\tools\busybox.exe
function Ensure-BusyBox {
  param(
    [string]$ToolsDir,
    [string]$Url = ""
  )

  New-Item -ItemType Directory -Force -Path $ToolsDir | Out-Null
  $outPath = Join-Path $ToolsDir "busybox.exe"

  if (Test-Path $outPath) {
    Write-Host "BusyBox: $outPath (cached)"
    return $outPath
  }

  if (-not $Url) {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq "ARM64") {
      $Url = "https://frippery.org/files/busybox/busybox64a.exe"
    } else {
      $Url = "https://frippery.org/files/busybox/busybox64u.exe"
    }
  }

  Write-Host "Downloading BusyBox from $Url ..."
  Invoke-WebRequest -Uri $Url -OutFile $outPath -ErrorAction Stop | Out-Null
  Write-Host "BusyBox: $outPath"
  return $outPath
}

$toolsDir = Join-Path $dist "tools"
$busyboxExe = Ensure-BusyBox -ToolsDir $toolsDir

# Portable "full" bundle (zip): titanbash.exe + tools\busybox.exe + notices.
$portableZip = Join-Path $dist "titanbash-$version-portable.zip"
$staging = Join-Path $dist ("titanbash-" + $version + "-portable")
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $staging
New-Item -ItemType Directory -Force -Path (Join-Path $staging "tools") | Out-Null
Copy-Item -Force $srcExe (Join-Path $staging "titanbash.exe")
Copy-Item -Force $busyboxExe (Join-Path $staging "tools\\busybox.exe")
if (Test-Path (Join-Path $repoRoot "LICENSE")) {
  Copy-Item -Force (Join-Path $repoRoot "LICENSE") (Join-Path $staging "LICENSE")
}
if (Test-Path (Join-Path $repoRoot "GPL-2.0.txt")) {
  Copy-Item -Force (Join-Path $repoRoot "GPL-2.0.txt") (Join-Path $staging "GPL-2.0.txt")
}
if (Test-Path (Join-Path $repoRoot "THIRD_PARTY_NOTICES.md")) {
  Copy-Item -Force (Join-Path $repoRoot "THIRD_PARTY_NOTICES.md") (Join-Path $staging "THIRD_PARTY_NOTICES.md")
}
if (Test-Path (Join-Path $repoRoot "README.md")) {
  Copy-Item -Force (Join-Path $repoRoot "README.md") (Join-Path $staging "README.md")
}
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $portableZip -Force
Write-Host "Portable (full): $portableZip"

if ($SkipInstaller) {
  Write-Host "SkipInstaller set; done."
  exit 0
}

function Find-Iscc {
  param([string]$Explicit)
  if ($Explicit -and (Test-Path $Explicit)) { return $Explicit }
  $cmd = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
  if ($cmd) { return $cmd.Source }
  $candidates = @(
    "${env:ProgramFiles(x86)}\\Inno Setup 6\\ISCC.exe",
    "${env:ProgramFiles}\\Inno Setup 6\\ISCC.exe"
  )
  foreach ($c in $candidates) {
    if (Test-Path $c) { return $c }
  }
  return ""
}

$iscc = Find-Iscc -Explicit $IsccPath
if (-not $iscc) {
  Write-Host "Inno Setup not found (ISCC.exe). Skipping installer build."
  Write-Host "Install Inno Setup 6+ and re-run, or pass -IsccPath."
  exit 0
}

Write-Host "Building installer with: $iscc"
& $iscc (Join-Path $repoRoot "installer\\titan-bash.iss") "/DMyAppVersion=$version"
if ($LASTEXITCODE -ne 0) {
  throw "ISCC failed with exit code $LASTEXITCODE"
}

Write-Host "Installer output: $dist"
Write-Host "Note: PATH changes only apply to new terminals."
