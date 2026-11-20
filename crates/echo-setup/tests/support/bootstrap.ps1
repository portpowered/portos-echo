# T3 external download substitute. No network, certificates or adb.
Import-Module Microsoft.PowerShell.Utility -ErrorAction Stop
function Invoke-WebRequest {
    param($Uri, $OutFile, [switch]$PassThru, [switch]$UseBasicParsing,
          $MaximumRedirection, $TimeoutSec, $ErrorAction)
    if ($MaximumRedirection -ne 0 -or $TimeoutSec -le 0 -or $TimeoutSec -gt 120) { throw 'SYNTHETIC-TOKEN' }
    $root = $env:ECHO_SETUP_TEST_BOOTSTRAP
    [IO.File]::AppendAllText((Join-Path $root 'requests'), $Uri.AbsoluteUri + "`n")
    $mode = [IO.File]::ReadAllText((Join-Path $root 'mode'))
    $asset = [IO.Path]::GetFileName($Uri.AbsolutePath)
    if ($mode.StartsWith('checksum-')) {
        $mode = if ($asset -eq 'SHA256SUMS') { $mode.Substring(9) } else { 'good' }
    }
    if ($mode -eq 'failure') { throw 'SYNTHETIC-TOKEN SYNTHETIC-ACCOUNT SYNTHETIC-PRIVATE-PATH' }
    $location = $null
    if ($mode -eq 'loop') { $location = $Uri.AbsoluteUri }
    elseif ($mode -eq 'downgrade' -or ($mode -eq 'later-downgrade' -and $Uri.AbsolutePath.StartsWith('/hop/'))) {
        $location = 'http://127.0.0.1/' + $asset
    } elseif ($mode -in @('redirect', 'later-downgrade') -and -not $Uri.AbsolutePath.StartsWith('/hop/')) {
        $location = '/hop/' + $asset
    }
    if ($location) { return @{ StatusCode = 302; Headers = @{ Location = $location } } }
    [IO.File]::Copy((Join-Path $root $asset), $OutFile, $true)
    return @{ StatusCode = 200 }
}
$bootstrap = Join-Path $env:ECHO_SETUP_TEST_BOOTSTRAP 'install.ps1'
if ($env:ECHO_SETUP_TEST_PIPE -eq '1') {
    # Invoke-Expression receives the actual rendered source as irm|iex does.
    # Bind the invocation's argv to a child scope, including paths with metacharacters.
    & { Invoke-Expression ([IO.File]::ReadAllText($bootstrap)) } @args
} else {
    & $bootstrap @args
    exit $LASTEXITCODE
}
