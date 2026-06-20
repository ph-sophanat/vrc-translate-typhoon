# Package vrc-translate-typhoon into a standalone Windows bundle that needs
# neither Rust nor Python on the target machine.
#
#   powershell -ExecutionPolicy Bypass -File package.ps1
#
# Produces:
#   dist-bundle\vrc-translate-typhoon\        (the runnable folder)
#   dist-bundle\vrc-translate-typhoon.zip     (zip of the same)
#
# Steps: build the Rust app (release), freeze the Python service with PyInstaller,
# then assemble both plus config into one folder.

$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$out  = Join-Path $root "dist-bundle\vrc-translate-typhoon"

Write-Host "==> Building Rust app (release)..." -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "==> Freezing Python service (PyInstaller)..." -ForegroundColor Cyan
& "$root\service\.venv\Scripts\pyinstaller.exe" "$root\service\build_server.spec" `
    --noconfirm --distpath "$root\service\dist" --workpath "$root\service\build_pyi"
if ($LASTEXITCODE -ne 0) { throw "pyinstaller failed" }

Write-Host "==> Assembling bundle at $out ..." -ForegroundColor Cyan
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Path $out -Force | Out-Null
New-Item -ItemType Directory -Path "$out\service" -Force | Out-Null

Copy-Item "$root\target\release\vrc-translate-typhoon.exe" $out
# Ship the key-less template as config.toml — the recipient fills in their own
# personal DeepL/Anthropic keys. NEVER bundle the real config.toml.
Copy-Item "$root\config.example.toml" "$out\config.toml"
Copy-Item "$root\config.example.toml" $out
Copy-Item "$root\README.md"   $out -ErrorAction SilentlyContinue
# PyInstaller onedir output: service\dist\server\{server.exe,_internal\...}
Copy-Item "$root\service\dist\server\*" "$out\service" -Recurse

$zip = Join-Path $root "dist-bundle\vrc-translate-typhoon.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Write-Host "==> Zipping to $zip ..." -ForegroundColor Cyan
Compress-Archive -Path $out -DestinationPath $zip

$size = "{0:N1} GB" -f ((Get-ChildItem $out -Recurse -File | Measure-Object Length -Sum).Sum / 1GB)
Write-Host "==> Done. Bundle is $size at:`n    $out`n    $zip" -ForegroundColor Green
