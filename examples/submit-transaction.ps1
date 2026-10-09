# PowerShell 7. A signed request is the durable retry unit; never change it here.
param(
    [Parameter(Mandatory)][string]$SecondExe,
    [Parameter(Mandatory)][string]$TransactionFile,
    [Parameter(Mandatory)][string]$AuthorizerPublicKey,
    [Parameter(Mandatory)][string]$EndpointsFile,
    [ValidateRange(1, 100)][int]$MaxAttempts = 24,
    [ValidateRange(1, 60)][int]$RetrySeconds = 1
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$endpoints = @(Get-Content -LiteralPath $EndpointsFile -Raw | ConvertFrom-Json)
if ($endpoints.Count -eq 0) { throw 'At least one pinned Validator endpoint is required.' }
foreach ($endpoint in $endpoints) {
    if (-not $endpoint.address -or -not $endpoint.certificate_base64) {
        throw 'Each endpoint requires address and certificate_base64.'
    }
}
# Freeze the exact input bytes for this invocation, even if the source is edited.
$retryFile = [System.IO.Path]::GetTempFileName()
try {
    [System.IO.File]::WriteAllBytes($retryFile, [System.IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $TransactionFile).Path))
    for ($attempt = 0; $attempt -lt $MaxAttempts; $attempt++) {
        $endpoint = $endpoints[$attempt % $endpoints.Count]
        $common = @($endpoint.address, $retryFile, $AuthorizerPublicKey, $endpoint.certificate_base64)
        $status = (& $SecondExe task-status @common 2>&1 | Out-String)
        $statusExit = $LASTEXITCODE
        if ($statusExit -eq 0 -and $status -match 'state=succeeded\s*$') {
            Write-Output $status.Trim()
            return
        }
        if ($statusExit -eq 0 -and $status -match 'state=cancelled\s*$') {
            throw ('Task reached certified cancellation: ' + $status.Trim())
        }
        if ($status -match 'LegalTask status query was rejected|invalid transaction request|invalid authorizer public key') {
            throw $status.Trim()
        }
        if ($statusExit -ne 0 -or $status -match 'state=(unknown|bound)\s*$') {
            $submitted = (& $SecondExe submit @common 2>&1 | Out-String)
            $submitExit = $LASTEXITCODE
            if ($submitExit -eq 0 -and $submitted -match 'state=succeeded\s*$') {
                Write-Output $submitted.Trim()
                return
            }
            if ($submitted -match 'LegalTask submission was rejected|invalid transaction request|invalid authorizer public key') {
                throw $submitted.Trim()
            }
            Write-Verbose $submitted.Trim()
        }
        if ($attempt + 1 -lt $MaxAttempts) { Start-Sleep -Seconds $RetrySeconds }
    }
    throw 'Retry budget exhausted; outcome remains uncertain. Preserve the original signed request and query or retry it later.'
} finally {
    Remove-Item -LiteralPath $retryFile -Force
}
