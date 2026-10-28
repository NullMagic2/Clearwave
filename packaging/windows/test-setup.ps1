# Validate setup syntax and recovery transactions without touching the registry.
# A memory-backed registry exercises the same functions used by the installer.
# This does not establish elevated installation or native audio-host compatibility.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Join-Path $PSScriptRoot 'setup-apo.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
foreach ($function in $ast.FindAll({param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst]}, $false)) {
    . ([scriptblock]::Create($function.Extent.Text))
}

class MemoryKey {
    [hashtable]$Values = @{}
    [string[]] GetValueNames() { return [string[]]@($this.Values.Keys) }
    [object] GetValue([string]$name, [object]$fallback, [object]$options) {
        if ($this.Values.ContainsKey($name)) { return $this.Values[$name].Value }
        return $fallback
    }
    [Microsoft.Win32.RegistryValueKind] GetValueKind([string]$name) { return $this.Values[$name].Kind }
    [void] SetValue([string]$name, [object]$value, [Microsoft.Win32.RegistryValueKind]$kind) {
        $this.Values[$name] = @{Value = $value; Kind = $kind}
    }
    [void] DeleteValue([string]$name, [bool]$throwMissing) { $this.Values.Remove($name) }
    [void] Dispose() {}
}
class MemoryRegistry {
    [hashtable]$Keys = @{}
    [MemoryKey] OpenSubKey([string]$path) { return $this.Keys[$path] }
    [MemoryKey] OpenSubKey([string]$path, [bool]$write) { return $this.Keys[$path] }
    [MemoryKey] CreateSubKey([string]$path) {
        if (-not $this.Keys.ContainsKey($path)) { $this.Keys[$path] = [MemoryKey]::new() }
        return $this.Keys[$path]
    }
}
$machine = [MemoryRegistry]::new()
Write-Value 'test' 'existing' 'MultiString' @('first', 'second')
$changes = @((Change 'test' 'existing' 'MultiString' @('replacement')), (Change 'test' 'new' 'DWord' 0))
foreach ($item in $changes) { Write-Value $item.Path $item.Name $item.Kind $item.Value }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString() + '.xml')
try {
    Save-State ([pscustomobject]@{Changes = $changes}) $temporary
    $saved = Import-Clixml -LiteralPath $temporary
    Restore $saved.Changes
    if (((Read-Value 'test' 'existing') -join ',') -ne 'first,second') { throw 'Original array was not restored.' }
    if ($null -ne (Read-Value 'test' 'new')) { throw 'New value was not removed.' }
    Restore $saved.Changes

    # Failure halfway through installation leaves some values untouched.
    # Recovery must accept these original values and restore applied changes.
    # An external change must instead abort and retain the recovery snapshot.
    $item = $changes[0]
    Write-Value $item.Path $item.Name $item.Kind $item.Value
    Restore $changes
    Write-Value 'test' 'existing' 'MultiString' @('other-software')
    $refused = $false
    try { Restore $changes } catch { $refused = $true }
    if (-not $refused) { throw 'External modification was overwritten.' }
    if ((Read-Value 'test' 'existing') -ne 'other-software') { throw 'External data changed.' }
} finally {
    Remove-Item -LiteralPath $temporary -ErrorAction SilentlyContinue
}
Write-Output 'Setup parser and recovery tests passed; no real registry changes performed.'
