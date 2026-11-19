# Builds the release program and writes dist\xmouse-psnbridge.zip, the file XMouse's
# "Install PSNBridge" button downloads from the latest GitHub release. Layout:
#   psnbridge\plugin.json
#   psnbridge\xmouse-psnbridge.exe
#   psnbridge\LICENSE
#   psnbridge\README.md
# Extracting it into XMouse's plugins folder is also the manual install.
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
  # cargo reports progress on stderr, which Windows PowerShell would treat as an error here.
  cmd /c "cargo +stable-x86_64-pc-windows-msvc build --release 2>&1"
  if ($LASTEXITCODE -ne 0) { throw "build failed" }

  New-Item -ItemType Directory -Force (Join-Path $root "dist") | Out-Null
  $zip = Join-Path $root "dist\xmouse-psnbridge.zip"
  if (Test-Path $zip) { Remove-Item -Force $zip }
  # Entries are named by hand: Windows PowerShell's ZipFile.CreateFromDirectory writes
  # backslashes, which zip tools (and XMouse's installer) don't accept as folder separators.
  Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
  $archive = [IO.Compression.ZipFile]::Open($zip, [IO.Compression.ZipArchiveMode]::Create)
  try {
    foreach ($f in "target\release\xmouse-psnbridge.exe", "plugin.json", "LICENSE", "README.md") {
      $name = "psnbridge/" + (Split-Path $f -Leaf)
      [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, (Join-Path $root $f), $name, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
    }
  } finally {
    $archive.Dispose()
  }

  $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
  "wrote $zip ($((Get-Item $zip).Length) bytes)"
  "sha256 $hash"
} finally {
  Pop-Location
}
