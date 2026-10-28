#Requires -Version 5.1
#Requires -RunAsAdministrator
<#
Attach the capture APO to one existing microphone, or restore its saved settings.
This development installer uses endpoint registration and preserves OEM effects.
It never installs a kernel driver, enables test mode, or changes audio protection.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^\{[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\}$')][string]$Endpoint,
    [ValidateSet('Install', 'Uninstall')][string]$Action = 'Install',
    [ValidatePattern('^[0-9a-f-]{1,64}$')][string]$RequestId = ''
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not [Environment]::Is64BitProcess) { throw 'Run this script with 64-bit PowerShell.' }

$Endpoint = $Endpoint.ToLowerInvariant()
$class = '{657B7D18-8316-4DB1-9585-B981D14E615B}'
$mode = '{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},6'
$modes = '{d3993a3f-99c2-4402-b5ec-a92a0367664b},6'
$association = '{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},0'
$disabled = '{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5'
$capture = "SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture\$Endpoint"
$fx = "$capture\FxProperties"
$com = "SOFTWARE\Classes\CLSID\$class"
$engine = "SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\$class"
$root = Join-Path $env:ProgramFiles 'Clearwave\APO'
$stateFile = Join-Path $root "$Endpoint.xml"
$globalFile = Join-Path $root 'registration.xml'
$machine = [Microsoft.Win32.Registry]::LocalMachine

function Read-Value([string]$Path, [string]$Name) {
    $key = $machine.OpenSubKey($Path)
    try {
        if ($null -eq $key -or $key.GetValueNames() -notcontains $Name) { return $null }
        return $key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally { if ($null -ne $key) { $key.Dispose() } }
}
function Change([string]$Path, [string]$Name, [string]$Kind, $Value) {
    $key = $machine.OpenSubKey($Path)
    try {
        $present = $null -ne $key -and $key.GetValueNames() -contains $Name
        return [pscustomobject]@{
            Path = $Path; Name = $Name; Kind = $Kind; Value = $Value
            Present = $present; KeyPresent = ($null -ne $key)
            OldKind = $(if ($present) { $key.GetValueKind($Name).ToString() } else { 'String' })
            OldValue = $(if ($present) { $key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } else { $null })
        }
    } finally { if ($null -ne $key) { $key.Dispose() } }
}
function Write-Value([string]$Path, [string]$Name, [string]$Kind, $Value) {
    $key = $machine.CreateSubKey($Path)
    try {
        if ($Kind -eq 'MultiString') { $Value = [string[]]$Value }
        if ($Kind -eq 'DWord') { $Value = [int]$Value }
        $key.SetValue($Name, $Value, [Microsoft.Win32.RegistryValueKind]::$Kind)
    } finally { $key.Dispose() }
}
function Save-State($State, [string]$Path) {
    $State | Export-Clixml -LiteralPath "$Path.tmp" -Depth 8
    Move-Item -LiteralPath "$Path.tmp" -Destination $Path -Force
}
function Restore($Changes) {
    foreach ($item in @($Changes)[(@($Changes).Count - 1)..0]) {
        $current = Read-Value $item.Path $item.Name
        # Restore only our own value or an already-restored original value.
        # Driver updates can replace endpoint properties while Clearwave is installed.
        # Refuse to erase those external changes and keep the recovery snapshot.
        if (($current | ConvertTo-Json -Compress) -ne ($item.Value | ConvertTo-Json -Compress)) {
            if (($current | ConvertTo-Json -Compress) -eq ($item.OldValue | ConvertTo-Json -Compress)) { continue }
            throw "Value changed externally: $($item.Path) / $($item.Name). Recovery snapshot retained."
        }
        if ($item.Present) {
            Write-Value $item.Path $item.Name $item.OldKind $item.OldValue
        } else {
            $key = $machine.OpenSubKey($item.Path, $true)
            try { if ($null -ne $key) { $key.DeleteValue($item.Name, $false) } }
            finally { if ($null -ne $key) { $key.Dispose() } }
        }
    }
}

function Write-SetupResult([bool]$Success, [string]$Message) {
    if ([string]::IsNullOrEmpty($RequestId)) { return }
    $maximumMessageCharacters = 2048
    if ($Message.Length -gt $maximumMessageCharacters) { $Message = $Message.Substring(0, $maximumMessageCharacters) }
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    $path = Join-Path $root "setup-$Endpoint.json"
    $temporary = Join-Path $root "setup-$Endpoint-$RequestId.tmp"
    $json = @{request_id = $RequestId; success = $Success; message = $Message} | ConvertTo-Json -Compress
    [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $path -Force
}

function Invoke-Setup {
if ($Action -eq 'Uninstall') {
    if (-not (Test-Path -LiteralPath $stateFile)) { throw 'No saved installation exists for this microphone.' }
    $state = Import-Clixml -LiteralPath $stateFile
    Restore $state.Changes
    Remove-Item -LiteralPath $stateFile
    if (@(Get-ChildItem -LiteralPath $root -Filter '{*.xml').Count -eq 0 -and (Test-Path -LiteralPath $globalFile)) {
        $registration = Import-Clixml -LiteralPath $globalFile
        Restore $registration.Changes
        Remove-Item -LiteralPath $globalFile
    }
    Write-Output 'Clearwave detached. Reopen recording apps; restart Windows if the old audio graph remains loaded.'
    return
}

$endpointKey = $machine.OpenSubKey($capture)
if ($null -eq $endpointKey) { throw 'The selected capture endpoint no longer exists.' }
$endpointKey.Dispose()
if (Test-Path -LiteralPath $stateFile) {
    throw 'This microphone already has a saved setup. Uninstall it before reinstalling or upgrading.'
}
foreach ($property in @($mode, '{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},1', '{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},2')) {
    $existing = Read-Value $fx $property
    if ($null -ne $existing -and "$existing" -ne '' -and "$existing" -ne '{00000000-0000-0000-0000-000000000000}') {
        throw 'This microphone already has mode or legacy effects. Clearwave will not replace the manufacturer processing. OEM integration is required.'
    }
}
$source = Join-Path $PSScriptRoot 'clearwave_apo.dll'
if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw 'Extract the complete package; clearwave_apo.dll is missing.' }
$hash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
$installedDirectory = Join-Path $root $hash
$dll = Join-Path $installedDirectory 'clearwave_apo.dll'
$existingRegistration = Test-Path -LiteralPath $globalFile
if ($existingRegistration) {
    $registration = Import-Clixml -LiteralPath $globalFile
    if ($registration.Hash -ne $hash) { throw 'Detach all microphones before upgrading the APO DLL.' }
} elseif ($null -ne (Read-Value "$com\InprocServer32" '')) {
    throw 'An untracked COM registration exists for Clearwave. It will not be overwritten.'
}
if ((Get-AuthenticodeSignature -LiteralPath $source).Status -ne 'Valid') {
    Write-Warning 'Development DLL is unsigned. Protected Windows audio hosts can reject it. This script does not disable audio protection.'
}

New-Item -ItemType Directory -Path $installedDirectory -Force | Out-Null
if (-not (Test-Path -LiteralPath $dll)) { Copy-Item -LiteralPath $source -Destination $dll }
if ((Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash -ne $hash) { throw 'Installed DLL checksum mismatch.' }
$globalChanges = @(
    (Change $com '' 'String' 'Clearwave microphone processing')
    (Change "$com\InprocServer32" '' 'String' $dll)
    (Change "$com\InprocServer32" 'ThreadingModel' 'String' 'Both')
    (Change $engine 'FriendlyName' 'String' 'Clearwave microphone processing')
    (Change $engine 'Copyright' 'String' 'Clearwave contributors')
    (Change $engine 'MajorVersion' 'DWord' 1)
    (Change $engine 'MinorVersion' 'DWord' 0)
    (Change $engine 'Flags' 'DWord' 15)
    (Change $engine 'MinInputConnections' 'DWord' 1)
    (Change $engine 'MaxInputConnections' 'DWord' 1)
    (Change $engine 'MinOutputConnections' 'DWord' 1)
    (Change $engine 'MaxOutputConnections' 'DWord' 1)
    (Change $engine 'MaxInstances' 'DWord' -1)
    (Change $engine 'NumAPOInterfaces' 'DWord' 1)
    (Change $engine 'APOInterface0' 'String' '{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}')
)
$changes = @(
    (Change $fx $modes 'MultiString' @('{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}', '{98951333-B9CD-48B1-A0A3-FF40682D73F7}'))
    (Change $fx $disabled 'DWord' 0)
)
if ($null -eq (Read-Value $fx $association)) {
    $changes += Change $fx $association 'String' '{00000000-0000-0000-0000-000000000000}'
}
# Attach the class last, after registration and mode properties are complete.
# Save recovery records before changing registry values so interruption is recoverable.
# The same saved transaction is used by explicit uninstall and error rollback.
$changes += Change $fx $mode 'String' $class
Save-State ([pscustomobject]@{ Changes = $changes }) $stateFile
if (-not $existingRegistration) {
    Save-State ([pscustomobject]@{ Hash = $hash; Changes = $globalChanges }) $globalFile
}
try {
    if (-not $existingRegistration) {
        foreach ($item in $globalChanges) { Write-Value $item.Path $item.Name $item.Kind $item.Value }
    }
    foreach ($item in $changes) { Write-Value $item.Path $item.Name $item.Kind $item.Value }
} catch {
    $failure = $_
    Restore $changes
    Remove-Item -LiteralPath $stateFile
    if (-not $existingRegistration) {
        Restore $globalChanges
        Remove-Item -LiteralPath $globalFile
    }
    throw $failure
}
Write-Output 'Registered for this microphone. Use 48000 Hz mono/stereo and enable audio enhancements.'
Write-Output 'Reopen recording apps (or restart Windows), then click Start in Clearwave. Select the original microphone in other apps.'
Write-Output 'Registration does not prove Windows loaded the APO. Check that the Clearwave signal bar responds while an app records.'
Write-Output "Undo: .\setup-apo.ps1 -Action Uninstall -Endpoint '$Endpoint'"

}
try {
    Invoke-Setup
    Write-SetupResult $true 'Microphone processing registered.'
    exit 0
} catch {
    $message = $_.Exception.Message
    try { Write-SetupResult $false $message } catch { Write-Error $_ -ErrorAction Continue }
    Write-Error $message -ErrorAction Continue
    exit 1
}
