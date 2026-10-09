# Foreground Windows operator supervisor for an already initialized deployment.
param(
    [Parameter(Mandatory)][string]$SecondExe,
    [Parameter(Mandatory)][string]$ConfigFile,
    [Parameter(Mandatory)][string]$DeploymentDirectory,
    [ulong[]]$ValidatorIds = @(),
    [ValidateRange(0, 86400)][int]$RunSeconds = 0
)
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This supervisor uses Windows hidden processes; use a host service manager on other systems.' }
$executable = (Resolve-Path -LiteralPath $SecondExe).Path
$deployment = (Resolve-Path -LiteralPath $DeploymentDirectory).Path
$config = Get-Content -LiteralPath $ConfigFile -Raw | ConvertFrom-Json
$selected = @($config.validators | Where-Object { $ValidatorIds.Count -eq 0 -or $_.validator_id -in $ValidatorIds })
if ($selected.Count -eq 0) { throw 'No configured Validator selected.' }
if ($ValidatorIds.Count -gt 0 -and @($selected.validator_id | Sort-Object -Unique).Count -ne @($ValidatorIds | Sort-Object -Unique).Count) {
    throw 'A selected ValidatorId is missing from the config.'
}
$children = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()
$run = [Guid]::NewGuid().ToString('N')
$endpoints = @()
try {
    foreach ($validator in $selected) {
        $id = [ulong]$validator.validator_id
        $base = Join-Path $deployment "validator-$id/second"
        # CLI validates the snapshot/capabilities and acquires its OS runtime lock.
        $stdout = "$base.operator-$run.stdout.log"
        $stderr = "$base.operator-$run.stderr.log"
        $arguments = @('node', [string]$validator.listen_address, ('"' + $base + '"'))
        $child = Start-Process -FilePath $executable -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
        $children.Add($child)
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        $line = ''
        while (-not $line) {
            if ($child.HasExited) { throw "Validator $id exited during startup. Inspect $stderr" }
            if ([DateTime]::UtcNow -ge $deadline) { throw "Validator $id startup deadline exceeded. Inspect $stderr" }
            if (Test-Path -LiteralPath $stdout) { $line = Get-Content -LiteralPath $stdout -TotalCount 1 }
            if (-not $line) { Start-Sleep -Milliseconds 100 }
        }
        $fields = $line.Trim() -split '\s+'
        if ($fields.Count -ne 8 -or $fields[0] -ne 'LISTENING' -or $fields[1] -ne $validator.listen_address -or $fields[2] -ne 'NODE' -or $fields[4] -ne 'CERT' -or $fields[6] -ne 'VALIDATOR' -or $fields[7] -ne [string]$id) {
            throw "Validator $id startup identity/endpoint does not match config: $line"
        }
        $endpoints += @{ address=$fields[1]; certificate_base64=$fields[5] }
        Write-Output $line
    }
    $inventory = Join-Path $deployment "operator-$run.endpoints.json"
    New-Item -ItemType File -Path $inventory -Value (ConvertTo-Json -InputObject $endpoints -Depth 4) | Out-Null
    Write-Output "ENDPOINTS $inventory"
    $started = [DateTime]::UtcNow
    while ($RunSeconds -eq 0 -or ([DateTime]::UtcNow - $started).TotalSeconds -lt $RunSeconds) {
        foreach ($child in $children) {
            if ($child.HasExited) { throw "A managed node exited (PID $($child.Id)); inspect this run's stderr logs." }
        }
        Start-Sleep -Seconds 1
    }
} finally {
    # Only handles created by this invocation; no PID file or process-name kill.
    foreach ($child in $children) {
        if (-not $child.HasExited) { $child.Kill(); $child.WaitForExit() }
        $child.Dispose()
    }
}
