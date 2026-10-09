#Requires -Version 7.3
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Install','Start','Stop','Restart','Status','Remove','Plan')][string]$Action,
    [Parameter(Mandatory)][ValidatePattern('^Second-[A-Za-z0-9_-]{1,48}$')][string]$Name,
    [string]$SecondExe, [string]$SnapshotBase, [string]$ListenAddress
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Use linux-service.py on Linux.' }
$marker = 'Managed by Second windows-service.ps1'
function Native([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}
if ($Action -in @('Install','Plan')) {
    if (-not $SecondExe -or -not $SnapshotBase -or -not $ListenAddress) { throw 'Binary, snapshot base and listen address are required.' }
    $exe = (Resolve-Path -LiteralPath $SecondExe).Path
    $data = (Resolve-Path -LiteralPath (Split-Path -Parent $SnapshotBase)).Path
    $base = Join-Path $data (Split-Path -Leaf $SnapshotBase)
    $program = Split-Path -Parent $exe
    foreach ($directory in @($data,$program)) {
        if ($directory.TrimEnd('\','/') -eq [IO.Path]::GetPathRoot($directory).TrimEnd('\','/')) { throw 'A drive root cannot be the program/data permission boundary.' }
    }
    if ($exe.StartsWith($data.TrimEnd('\') + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Keep the program outside the writable node directory.' }
    $logs = Join-Path $data "$Name-logs"
    foreach ($value in @($exe,$base,$logs,$ListenAddress)) {
        if ($value -match '["\r\n\x00]') { throw 'Quotes/control characters are not allowed in service arguments.' }
    }
    Native $exe @('node-check',$ListenAddress,$base)
    $image = '"' + $exe + '" service ' + $Name + ' ' + $ListenAddress + ' "' + $base + '" "' + $logs + '"'
    if ($Action -eq 'Plan') { Write-Output $image; return }
}
$admin = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($Action -ne 'Status' -and -not $admin) { throw 'Run this operation in an Administrator PowerShell. No service was changed.' }
$existing = Get-CimInstance Win32_Service -Filter "Name='$Name'"
if ($Action -eq 'Install') {
    if ($existing) { throw 'Service already exists; refusing to replace it.' }
    $created = $false
    try {
        Native sc.exe @('create',$Name,'binPath=',$image,'start=','delayed-auto','obj=',"NT SERVICE\$Name")
        $created = $true
        Native sc.exe @('description',$Name,$marker)
        Native sc.exe @('sidtype',$Name,'unrestricted')
        # Per-service virtual account; no LocalSystem or shared LocalService key access.
        Native icacls.exe @($data,'/grant',"NT SERVICE\${Name}:(OI)(CI)M",'/T','/Q')
        Native icacls.exe @($program,'/grant',"NT SERVICE\${Name}:(OI)(CI)RX",'/T','/Q')
        Native sc.exe @('failure',$Name,'reset=','86400','actions=','restart/5000/restart/15000/restart/60000')
        Native sc.exe @('failureflag',$Name,'1')
        Start-Service -Name $Name
        (Get-Service $Name).WaitForStatus('Running',[TimeSpan]::FromSeconds(30))
    } catch {
        if ($created) {
            Stop-Service -Name $Name -ErrorAction SilentlyContinue
            Native sc.exe @('delete',$Name)
        }
        throw
    }
} else {
    if (-not $existing -or $existing.Description -ne $marker) { throw 'Refusing to manage a service not installed by this tool.' }
    switch ($Action) {
        'Start' { Start-Service $Name; (Get-Service $Name).WaitForStatus('Running',[TimeSpan]::FromSeconds(30)) }
        'Stop' { Stop-Service $Name; (Get-Service $Name).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30)) }
        'Restart' { Restart-Service $Name; (Get-Service $Name).WaitForStatus('Running',[TimeSpan]::FromSeconds(30)) }
        'Remove' { Stop-Service $Name; (Get-Service $Name).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30)); Native sc.exe @('delete',$Name) }
    }
}
if ($Action -ne 'Remove') { Get-CimInstance Win32_Service -Filter "Name='$Name'" | Select-Object Name,State,StartMode,StartName,PathName,ProcessId }
