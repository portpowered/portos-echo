param()
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
function Get-Digest([string] $Path) {
    $stream = [IO.File]::OpenRead($Path)
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose(); $stream.Dispose() }
}
function Get-VerifiedBinary([string] $Published, [string] $ArchiveName, [string] $Expected, [string] $BinaryName) {
    $binary = Join-Path $Published $BinaryName
    $archive = Join-Path $Published $ArchiveName
    if ((Get-Digest $archive) -ne $Expected -or
        (Get-Digest $binary) -ne (Get-Content -LiteralPath (Join-Path $Published 'binary.sha256') -Raw).Trim()) { throw 'SHA-256 mismatch' }
    foreach ($notice in @('LICENSE', 'NOTICE')) {
        if (!(Test-Path -LiteralPath (Join-Path $Published $notice) -PathType Leaf)) { throw 'cached attribution missing' }
    }
    return $binary
}
try {
    if ($args.Count -ne 0) { throw 'takes no arguments' }
    $hostArchitecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
        $hostArchitecture -ne 'AMD64') { throw 'unsupported host' }
    $root = Split-Path $PSScriptRoot -Parent
    $commit = 'a956835d020762cb2b570053af06f643a11c0ecc'
    $target = 'x86_64-pc-windows-msvc'
    $expected = 'aad5cf364c873b148e462b58fd06ad7b37fcab9b996be67133386b9322df26bb'
    $archiveName = 'codex-x86_64-pc-windows-msvc.exe.zip'
    # Parse this deliberately small TOML vocabulary as data. Reject ambiguous keys.
    $pin = @{}
    $section = ''
    foreach ($line in Get-Content -LiteralPath (Join-Path $root 'codex.pin')) {
        if ($line -match '^\s*$') { continue }
        if ($line -match '^\[assets\.([a-z0-9_-]+)\]$') { $section = $Matches[1] + '.'; continue }
        if ($line -notmatch '^([a-z0-9_]+) = "([^"\r\n]+)"$') { throw 'invalid pin syntax' }
        $key = $section + $Matches[1]
        if ($pin.ContainsKey($key)) { throw 'duplicate pin key' }
        $pin[$key] = $Matches[2]
    }
    if ($pin.repo -ne 'https://github.com/openai/codex' -or $pin.tag -ne 'rust-v0.160.0' -or
        $pin.commit -ne $commit -or $pin["$target.name"] -ne $archiveName -or
        $pin["$target.sha256"] -ne $expected) { throw 'invalid pin' }
    $cache = Join-Path $env:LOCALAPPDATA 'portos-echo/codex'
    if ($env:ECHO_CODEX_CACHE_DIR) { $cache = $env:ECHO_CODEX_CACHE_DIR }
    if (![IO.Path]::IsPathRooted($cache)) { throw 'cache must be absolute' }
    $cache = [IO.Path]::GetFullPath($cache)
    # Python is a repository prerequisite and resolves junctions, including ancestors.
    $resolved = & python -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' $cache
    if ($LASTEXITCODE -ne 0) { throw 'cache canonicalization failed' }
    $checkout = $root
    while ($checkout -and !(Test-Path -LiteralPath (Join-Path $checkout '.git') -PathType Container)) {
        $checkout = Split-Path $checkout -Parent
    }
    foreach ($forbidden in @($root, $checkout)) {
        if ($forbidden -and ($cache -eq $forbidden -or $cache.StartsWith($forbidden + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
            $resolved -eq $forbidden -or $resolved.StartsWith($forbidden + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase))) { throw 'cache must be outside repository' }
    }
    $parent = Join-Path $cache "$commit/$target"
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $published = Join-Path $parent $expected
    $lock = $published + '.lock'
    $binaryName = "codex-$target.exe"
    if (Test-Path -LiteralPath $published) {
        Write-Output (Get-VerifiedBinary $published $archiveName $expected $binaryName)
        exit 0
    }
    if ($env:ECHO_CODEX_OFFLINE -eq '1') { throw 'binary missing; provision before testing' }
    # CreateNew owns the lock; another writer's lock is never removed.
    $lease = [IO.File]::Open($lock, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $binary = Join-Path $published $binaryName
        if (Test-Path -LiteralPath $published) {
            $binary = Get-VerifiedBinary $published $archiveName $expected $binaryName
        } else {
            $stage = Join-Path $parent ('fetch-' + [Guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Path $stage | Out-Null
            try {
                $archive = Join-Path $stage $archiveName
                Invoke-WebRequest ($pin.repo + '/releases/download/' + $pin.tag + '/' + $archiveName) -OutFile $archive -TimeoutSec 120 -UseBasicParsing
                if ((Get-Digest $archive) -ne $expected) { throw 'SHA-256 mismatch' }
                Add-Type -AssemblyName System.IO.Compression.FileSystem
                $zip = [IO.Compression.ZipFile]::OpenRead($archive)
                try {
                    foreach ($entry in $zip.Entries) {
                        $destination = [IO.Path]::GetFullPath((Join-Path $stage $entry.FullName))
                        if (!$destination.StartsWith($stage + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or $entry.FullName.Contains(':')) { throw 'archive path traversal' }
                    }
                } finally { $zip.Dispose() }
                Expand-Archive -LiteralPath $archive -DestinationPath $stage
                $stageBinary = Join-Path $stage $binaryName
                Get-Digest $stageBinary | Set-Content -LiteralPath (Join-Path $stage 'binary.sha256') -Encoding Ascii
                foreach ($notice in @('LICENSE', 'NOTICE')) {
                    Invoke-WebRequest ('https://raw.githubusercontent.com/openai/codex/' + $commit + '/' + $notice) -OutFile (Join-Path $stage $notice) -TimeoutSec 120 -UseBasicParsing
                }
                Move-Item -LiteralPath $stage -Destination $published
            } finally {
                if (Test-Path -LiteralPath $stage) {
                    $stageFull = [IO.Path]::GetFullPath($stage)
                    if (!$stageFull.StartsWith($parent + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'invalid staging cleanup path' }
                    Remove-Item -LiteralPath $stageFull -Recurse -Force
                }
            }
        }
        Write-Output $binary
    } finally { $lease.Dispose(); Remove-Item -LiteralPath $lock }
} catch { [Console]::Error.WriteLine('codex-app-server: ' + $_.Exception.Message); exit 1 }
