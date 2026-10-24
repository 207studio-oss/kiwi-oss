param(
    [string]$TargetDirectory = (Join-Path $env:USERPROFILE "Scratch\$(Get-Date -Format yyyy-MM-dd)-kiwi-sprouts-rust\target"),
    [string]$BinDirectory = (Join-Path $env:USERPROFILE '.local\bin')
)

$ErrorActionPreference = 'Stop'
Get-Command cargo -ErrorAction Stop | Out-Null
$manifest = Join-Path $PSScriptRoot 'Cargo.toml'
& cargo build --locked --release --bins --manifest-path $manifest --target-dir $TargetDirectory
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }
New-Item -ItemType Directory -Force -Path $BinDirectory | Out-Null
Copy-Item -LiteralPath (Join-Path $TargetDirectory 'release\kiwi-sprouts.exe') -Destination (Join-Path $BinDirectory 'kiwi-sprouts-rs.exe')
foreach ($name in @('kiwi-sprouts-test-driver', 'kiwi-sprouts-fixture')) {
    Copy-Item -LiteralPath (Join-Path $TargetDirectory "release\$name.exe") -Destination (Join-Path $BinDirectory "$name.exe")
}
Write-Output "Installed $(Join-Path $BinDirectory 'kiwi-sprouts-rs.exe'); MCP configuration still uses Python."
