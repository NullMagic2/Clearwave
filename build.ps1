# Build the native Windows application; keep deliverables in bin/.
[CmdletBinding()]
param([string]$BuildDirectory = (Join-Path $PSScriptRoot '.build'))
$ErrorActionPreference = 'Stop'

Push-Location $PSScriptRoot
try {
    $cargoCommand = if ($env:CARGO) { $env:CARGO } else { 'cargo' }
    & $cargoCommand build --locked --offline --release --features windows-gui --target-dir $BuildDirectory
    if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }

    $binaryDirectory = Join-Path $PSScriptRoot 'bin'
    New-Item -ItemType Directory -Force $binaryDirectory | Out-Null
    foreach ($name in @('clearwave.exe', 'clearwave_apo.dll')) {
        $temporaryBinary = Join-Path $binaryDirectory ('.new-' + $name)
        Copy-Item (Join-Path $BuildDirectory ('release/' + $name)) $temporaryBinary -Force
        Move-Item $temporaryBinary (Join-Path $binaryDirectory $name) -Force
        Write-Output "Built: $(Join-Path $binaryDirectory $name)"
    }
} finally {
    Pop-Location
}
