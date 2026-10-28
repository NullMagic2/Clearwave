# Build in an isolated temporary directory and retain only the executable output.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'

$temporaryBuild = Join-Path $PSScriptRoot ('.clean-build.' + [guid]::NewGuid().ToString('N'))
try {
    & (Join-Path $PSScriptRoot 'build.ps1') -BuildDirectory $temporaryBuild
    Remove-Item (Join-Path $PSScriptRoot '.build'), (Join-Path $PSScriptRoot 'target') -Recurse -Force -ErrorAction SilentlyContinue
} finally {
    if (Test-Path $temporaryBuild) { Remove-Item $temporaryBuild -Recurse -Force }
}
