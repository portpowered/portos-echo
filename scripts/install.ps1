# Paired release bootstrap. Works in Windows PowerShell 5.1 and PowerShell 7.
# Manual: .\install.ps1 -Version VERSION [-Config FILE]
# Paired release: irm https://github.com/portpowered/portos-echo/releases/download/vVERSION/install.ps1 | iex
# With irm|iex, set $env:ECHO_SETUP_CONFIG to the optional config file first.
# For an OpenAI API key, set $env:ECHO_SETUP_OPENAI_API_KEY_FILE to the key file's path (never the key).
function Get-EchoReleaseFile {
    param([Uri]$Uri, [string]$Destination)
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    for ($hop = 0; $hop -le 10; $hop++) {
        if ($Uri.Scheme -cne 'https' -or $Uri.UserInfo -or $elapsed.Elapsed.TotalSeconds -ge 120) {
            throw 'download failed'
        }
        $response = $null
        try {
            # Windows PowerShell emits a nonterminating redirect error and
            # returns the 3xx response. PowerShell 7 throws with the response.
            $response = Invoke-WebRequest -Uri $Uri -OutFile $Destination -PassThru `
                -UseBasicParsing -MaximumRedirection 0 -TimeoutSec ([int][Math]::Ceiling(120 - $elapsed.Elapsed.TotalSeconds)) `
                -ErrorAction SilentlyContinue
        } catch {
            $response = $_.Exception.Response
            if ($null -eq $response) { throw 'download failed' }
        }
        try {
            $status = [int]$response.StatusCode
            if ($status -eq 200) { return }
            if ($status -notin @(301, 302, 303, 307, 308) -or $hop -eq 10) { throw 'download failed' }
            # PowerShell 7 exceptions carry HttpResponseMessage; 5.1 uses HttpWebResponse.
            $location = if ($response.Headers.Location) { [string]$response.Headers.Location } else { [string]$response.Headers['Location'] }
            if (-not $location -or $location -match '\s') { throw 'download failed' }
            $Uri = [Uri]::new($Uri, $location)
        } finally {
            if ($response -is [IDisposable]) { $response.Dispose() }
        }
    }
    throw 'download failed'
}

function Invoke-EchoRelease {
    param([object[]]$Arguments)
    $version = '@ECHO_VERSION@'
    $config = $env:ECHO_SETUP_CONFIG
    $apiKeyFile = $env:ECHO_SETUP_OPENAI_API_KEY_FILE
    $seen = @{}
    for ($i = 0; $i -lt $Arguments.Count; $i++) {
        $flag = [string]$Arguments[$i]
        if ($flag -in @('-Help', '--help') -and $Arguments.Count -eq 1) {
            [Console]::Out.WriteLine('Usage: install.ps1 [-Version VERSION] [-Config FILE] [-OpenAIApiKeyFile FILE]')
            [Console]::Out.WriteLine('Windows x86_64; requires adb. Source bootstrap requires -Version.')
            [Console]::Out.WriteLine('With irm|iex, set ECHO_SETUP_CONFIG to the optional config file first.')
            [Console]::Out.WriteLine('Set ECHO_SETUP_OPENAI_API_KEY_FILE to an API key file path to skip ChatGPT sign-in.')
            return 0
        }
        $key = switch ($flag) { { $_ -in @('-Version', '--version') } { 'version' } { $_ -in @('-Config', '--config') } { 'config' } { $_ -in @('-OpenAIApiKeyFile', '--openai-api-key-file') } { 'apikey' } default { '' } }
        if (-not $key -or $seen.ContainsKey($key) -or $i + 1 -ge $Arguments.Count) { throw 'invalid usage' }
        $seen[$key] = $true
        $i++
        $value = [string]$Arguments[$i]
        if (-not $value -or $value.StartsWith('-')) { throw 'invalid usage' }
        if ($key -eq 'version') { $version = $value -creplace '^v', '' } elseif ($key -eq 'config') { $config = $value } else { $apiKeyFile = $value }
    }
    if ($version -cnotmatch '\A[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9]+(\.[A-Za-z0-9]+)*)?\z') { throw 'invalid usage' }
    if ($config -and -not [IO.File]::Exists($config)) { throw 'config is not readable' }
    if ($apiKeyFile -and -not [IO.File]::Exists($apiKeyFile)) { throw 'API key file is not readable' }
    Invoke-EchoVerifiedInstaller $version $config $apiKeyFile
}

function Invoke-EchoVerifiedInstaller {
    param([string]$Version, [string]$Config, [string]$ApiKeyFile)
    $temporary = $null
    try {
        $temporary = Join-Path ([IO.Path]::GetTempPath()) ('echo-setup-' + [Guid]::NewGuid().ToString('N'))
        $acl = [Security.AccessControl.DirectorySecurity]::new()
        $acl.SetAccessRuleProtection($true, $false)
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        $rule = [Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        $acl.AddAccessRule($rule)
        # Apply the protected ACL at creation, before any untrusted bytes arrive.
        $directory = [IO.DirectoryInfo]::new($temporary)
        if ($PSVersionTable.PSVersion.Major -ge 7) { [IO.FileSystemAclExtensions]::Create($directory, $acl) } else { $directory.Create($acl) }
        $asset = "echo-setup-$Version-windows-x86_64.exe"
        $binary = Join-Path $temporary $asset
        $checksums = Join-Path $temporary 'SHA256SUMS'
        $origin = "https://github.com/portpowered/portos-echo/releases/download/v$Version"
        Get-EchoReleaseFile ([Uri]"$origin/$asset") $binary
        Get-EchoReleaseFile ([Uri]"$origin/SHA256SUMS") $checksums
        $selected = @([IO.File]::ReadAllLines($checksums) | Where-Object { ($_ -split '\s+')[1] -ceq $asset })
        if ($selected.Count -ne 1 -or $selected[0] -cnotmatch ('\A([0-9a-f]{64})\s+' + [Regex]::Escape($asset) + '\z')) {
            throw 'invalid installer checksum'
        }
        $digest = $Matches[1]
        if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant() -cne $digest) {
            throw 'installer checksum mismatch'
        }
        $installerArguments = @('install', '--version', $Version)
        if ($Config) { $installerArguments += @('--config', $Config) }
        # Only the path crosses argv; echo-setup reads the key itself.
        if ($ApiKeyFile) { $installerArguments += @('--openai-api-key-file', $ApiKeyFile) }
        $global:LASTEXITCODE = $null
        $ErrorActionPreference = 'Continue'
        # Windows PowerShell otherwise decorates native stderr with this private
        # temporary script path. Preserve the child's already-sanitized text.
        & $binary @installerArguments 2>&1 | ForEach-Object {
            if ($_ -is [Management.Automation.ErrorRecord]) {
                if ($_.FullyQualifiedErrorId -notin @('NativeCommandError', 'NativeCommandErrorMessage')) { throw 'bootstrap failed' }
                [Console]::Error.WriteLine($_.ToString())
            }
            else { [Console]::Out.WriteLine([string]$_) }
        }
        if ($null -eq $LASTEXITCODE) { throw 'bootstrap failed' }
        return $LASTEXITCODE
    } finally {
        if ($temporary -and [IO.Directory]::Exists($temporary)) {
            Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction Stop
        }
    }
}

try {
    $result = Invoke-EchoRelease @($args)
    exit $result
} catch {
    $message = $_.Exception.Message
    if ($message -notin @('invalid usage', 'config is not readable', 'API key file is not readable', 'download failed', 'invalid installer checksum', 'installer checksum mismatch')) {
        $message = 'bootstrap failed'
    }
    [Console]::Error.WriteLine('echo-release: ' + $message)
    if ($message -eq 'invalid usage') { exit 2 }
    exit 1
}
