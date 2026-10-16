#Requires -Version 7.0
<#
Offline activation of the verified personal AetherShell build and DSH adapter.
-CheckOnly validates artifacts and reports blockers without writing files.
Actual activation requires an elevated external terminal and a stopped DSH Host.
This script never starts or stops DSH. Actual activation runs bounded candidate and installed-interpreter capability probes.
#>
[CmdletBinding()]
param(
    [switch] $CheckOnly,
    [ValidateRange(0, 2147483647)][int] $ExpectedHostProcessId = 0,
    [ValidateRange(1, 65535)][int] $WebPort = 3080,
    [string] $DshCheckout = 'C:\Project\deepseek-harness',
    [string] $DshHome = 'C:\Project\deepseek-harness-data',
    [string] $NativeBuild = (Join-Path $PSScriptRoot '..\target\release'),
    [string] $InstallDirectory = 'C:\Program Files\AetherShell'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$profileDirectory = Join-Path $DshHome 'profiles\web'
$patchPath = Join-Path $profileDirectory 'cordis.patch.yml'
$manifestPath = Join-Path $profileDirectory 'package.json'
$packageName = '@ryanzhou416/dsh-plugin-aether-shell'
$packageVersion = '0.1.0-dsh017rc1.5'
$archive = Join-Path $DshCheckout 'fork-plugins\releases\ryanzhou416-dsh-plugin-aether-shell-0.1.0-dsh017rc1.5.tgz'
$archiveHash = 'A43E09FAE2AE1EB0D55F9BA55707FEC42B0A6A3A13EA3AE3E8F848E3D49520DF'
$nativeHashes = [ordered]@{
    'ae.exe' = 'E5A4E86036EDB0714C57F6B4685E960C7AE18022D4DC007A5FA981409B08C08C'
    'aimodel.exe' = '1B0A3B2B07524D1E62921387AB80D481AFB8D3173E5A346EF21E2C07C33D4FB1'
}

function Assert-Hash([string] $Path, [string] $Expected) {
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actual -ne $Expected) { throw "Artifact hash differs: $Path. Rebuilds require new verification; do not bypass this check." }
}

function Get-Blockers {
    if ($ExpectedHostProcessId -gt 0) {
        try {
            $owner = [Diagnostics.Process]::GetProcessById($ExpectedHostProcessId)
            try { if (-not $owner.HasExited) { "The previous Host process $ExpectedHostProcessId is still alive." } }
            finally { $owner.Dispose() }
        } catch [ArgumentException] { } # No process owns that id.
        catch { "Cannot establish that Host PID $ExpectedHostProcessId exited: $($_.Exception.Message)" }
    }
    try {
        $listeners = @(Get-CimInstance -Namespace root/StandardCimv2 -ClassName MSFT_NetTCPConnection -Filter "LocalPort=$WebPort AND State=2" -ErrorAction Stop)
        foreach ($listener in $listeners) { "Port $WebPort is still listening (PID $($listener.OwningProcess))." }
    } catch { "Cannot verify the GUI listener is stopped: $($_.Exception.Message)" }
    try {
        $native = @(Get-CimInstance Win32_Process -Filter "Name='ae.exe' OR Name='aimodel.exe'" -ErrorAction Stop)
        foreach ($process in $native) { "A native interpreter is still running (PID $($process.ProcessId), $($process.Name))." }
    } catch { "Cannot verify interpreter ownership: $($_.Exception.Message)" }
}

function Assert-Offline {
    $blockers = @(Get-Blockers)
    if ($blockers.Count -gt 0) { throw ('Activation refused; close DSH and all AetherShell processes first. ' + ($blockers -join ' ')) }
}

function Read-Json([string] $Path) {
    Get-Content -LiteralPath $Path -Raw -Encoding utf8 | ConvertFrom-Json -AsHashtable
}

function Get-UpdatedPatch([string] $Text) {
    $blocks = [regex]::Matches($Text, '(?m)^- id:[ \t]*aether-shell[ \t]*\r?$')
    if ($blocks.Count -ne 1) { throw 'Expected exactly one top-level aether-shell override; inspect the profile manually.' }
    $start = $blocks[0].Index
    $following = [regex]::Match($Text.Substring($start + $blocks[0].Length), '(?m)^-(?:[ \t]|$)')
    $end = if ($following.Success) { $start + $blocks[0].Length + $following.Index } else { $Text.Length }
    $block = $Text.Substring($start, $end - $start)
    $config = [regex]::Matches($block, '(?m)^  config:[ \t]*\r?$')
    $executable = [regex]::Matches($block, '(?m)^    executable:[ \t]*([^\r\n]+)')
    if ($config.Count -ne 1 -or $executable.Count -ne 1) { throw 'Unexpected AetherShell config layout; no automatic patch is safe.' }
    $path = $executable[0].Groups[1].Value.Trim().Trim([char]39, [char]34)
    if ($path -ne (Join-Path $InstallDirectory 'ae.exe')) { throw 'The profile selects a different interpreter; inspect it before activation.' }
    $mode = [regex]::Matches($block, '(?m)^    structuredDataMode:[^\r\n]*')
    if ($mode.Count -gt 1) { throw 'Duplicate structuredDataMode settings; inspect the profile.' }
    if ($mode.Count -eq 1) {
        if ($mode[0].Value -notmatch '^    structuredDataMode:[ \t]*[\x27\x22]?(legacy|typed-v1)[\x27\x22]?[ \t]*$') {
            throw 'Unexpected structuredDataMode expression; no automatic replacement is safe.'
        }
        $block = $block.Remove($mode[0].Index, $mode[0].Length).Insert($mode[0].Index, '    structuredDataMode: typed-v1')
    } else {
        $newline = if ($block.Contains("`r`n")) { "`r`n" } else { "`n" }
        $at = $executable[0].Index + $executable[0].Length
        $block = $block.Insert($at, $newline + '    structuredDataMode: typed-v1')
    }
    $Text.Substring(0, $start) + $block + $Text.Substring($end)
}

function Assert-TypedCapabilities([string] $Executable) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $start.ArgumentList.Add('agent'); $start.ArgumentList.Add('execute')
    $child = [Diagnostics.Process]::new()
    $child.StartInfo = $start
    $started = $false
    try {
        $started = $child.Start()
        if (-not $started) { throw 'The capability process did not start.' }
        $stdout = $child.StandardOutput.ReadToEndAsync()
        $stderr = $child.StandardError.ReadToEndAsync()
        $child.StandardInput.WriteLine('{"action":"capabilities"}')
        $child.StandardInput.Close()
        if (-not $child.WaitForExit(15000)) { throw 'The capability probe exceeded its 15-second deadline.' }
        if ($child.ExitCode -ne 0) { throw 'The interpreter capability process failed.' }
        $document = $stdout.GetAwaiter().GetResult() | ConvertFrom-Json -AsHashtable
        $null = $stderr.GetAwaiter().GetResult()
        if ($document['success'] -ne $true) { throw 'The interpreter refused capability discovery.' }
        $cap = $document['result']
        if ($cap['protocol'] -cne 'aethershell-agent-api' -or $cap['protocol_version'] -ne 1) { throw 'Unsupported interpreter data protocol.' }
        foreach ($feature in @('typed_arguments_v1','typed_pipeline_input_v1','explicit_null_pipeline_input_v1')) {
            if ($cap['features'][$feature] -isnot [bool] -or $cap['features'][$feature] -cne $true) { throw "Required typed-data capability missing: $feature" }
        }
    } finally {
        try {
            if ($started -and -not $child.HasExited) { $child.Kill($true); if (-not $child.WaitForExit(5000)) { throw 'Capability-process cleanup did not settle.' } }
        } finally { $child.Dispose() }
    }
}

function Write-AtomicText([string] $Path, [string] $Text) {
    $temporary = Join-Path ([IO.Path]::GetDirectoryName($Path)) ('.aether-activation-' + [guid]::NewGuid().ToString('N') + '.tmp')
    try {
        $stream = [IO.FileStream]::new($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text); $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) }
        finally { $stream.Dispose() }
        [IO.File]::Move($temporary, $Path, $true)
    } finally { if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) } }
}

function Get-CanonicalJson($Value) {
    if ($null -eq $Value) { return 'null' }
    if ($Value -is [Collections.IDictionary]) {
        $fields = @($Value.Keys | Sort-Object -CaseSensitive | ForEach-Object { (ConvertTo-Json -InputObject ([string]$_) -Compress) + ':' + (Get-CanonicalJson $Value[$_]) })
        return '{' + ($fields -join ',') + '}'
    }
    if ($Value -is [Collections.IList]) { return '[' + (@($Value | ForEach-Object { Get-CanonicalJson $_ }) -join ',') + ']' }
    ConvertTo-Json -InputObject $Value -Depth 100 -Compress
}

function Get-UnrelatedManifestJson($Manifest) {
    $copy = $Manifest | ConvertTo-Json -Depth 100 | ConvertFrom-Json -AsHashtable
    if (-not $copy['dependencies'].ContainsKey($packageName)) { throw 'The AetherShell dependency disappeared from the profile manifest.' }
    $copy['dependencies'][$packageName] = '<aether-shell-package>'
    Get-CanonicalJson $copy
}

try {
if (-not $IsWindows) { throw 'This activation script is for this Windows installation only.' }
$hadDshHome = Test-Path Env:DSH_HOME
$oldDshHome = $env:DSH_HOME
if ($hadDshHome -and -not $PSBoundParameters.ContainsKey('DshHome') -and [IO.Path]::GetFullPath($oldDshHome).TrimEnd([char]92) -ne [IO.Path]::GetFullPath($DshHome).TrimEnd([char]92)) {
    throw "Inherited DSH_HOME selects $oldDshHome, not $DshHome. Select the intended home explicitly before activation."
}
foreach ($name in $nativeHashes.Keys) { Assert-Hash (Join-Path $NativeBuild $name) $nativeHashes[$name] }
Assert-Hash $archive $archiveHash
if (((Get-Item -LiteralPath $patchPath).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'The profile override is link-shaped; inspect it instead of replacing it.' }
$originalPatch = [IO.File]::ReadAllText($patchPath)
$updatedPatch = Get-UpdatedPatch $originalPatch
$beforeManifest = Read-Json $manifestPath
$beforeUnrelated = Get-UnrelatedManifestJson $beforeManifest
$priorHashes = @{
    'ae.exe' = @('356041AF4FE0889AEB6BE8C6B6FA99F193D5A240904C481D0E59D8BD30570C11', $nativeHashes['ae.exe'])
    'aimodel.exe' = @('4F0A4E2AFC578866CC994A74B7AF19223F0007D6F4DC48673048E140A567E2CF', $nativeHashes['aimodel.exe'])
}
$oldHashes = @{}
foreach ($name in $nativeHashes.Keys) {
    $oldHashes[$name] = (Get-FileHash -LiteralPath (Join-Path $InstallDirectory $name) -Algorithm SHA256).Hash
    if ($oldHashes[$name] -notin $priorHashes[$name]) { throw "Installed $name has an unverified hash: $($oldHashes[$name]). Inspect it before replacement." }
}
$pnpm = Join-Path $DshCheckout 'scripts\fork-windows-pnpm.cmd'
if (-not (Test-Path -LiteralPath $pnpm -PathType Leaf)) { throw 'The checkout-local pnpm launcher is missing.' }
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$administrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$blockers = @(Get-Blockers)
if ($CheckOnly) {
    [ordered]@{
        checkOnly = $true; filesChanged = $false; nativeVersion = '12.0.2+ryan.3'; pluginVersion = $packageVersion
        artifactHashesVerified = $true; typedCapabilitiesVerified = 'not-run'; administrator = $administrator
        offline = ($blockers.Count -eq 0); blockers = $blockers
        nextAction = 'Stop the old Host if blocked; otherwise continue the prepared run.cmd activation. This check alone installs nothing.'
    } | ConvertTo-Json -Depth 4
    if ($blockers.Count -gt 0) { exit 2 }
    exit 0
}
Assert-Offline
if (-not $administrator) { throw 'Open an external PowerShell 7 terminal as administrator, then run this script again after DSH has stopped.' }
Assert-TypedCapabilities (Join-Path $NativeBuild 'ae.exe')
} catch {
    if ($CheckOnly) {
        [ordered]@{ checkOnly=$true; filesChanged=$false; artifactHashesVerified=$false; typedCapabilitiesVerified='not-run'; offline=$false; blockers=@($_.Exception.Message) } | ConvertTo-Json -Depth 4
        exit 2
    }
    throw
}
$backup = Join-Path $DshHome ('backups\aether-activation-' + [datetime]::Now.ToString('yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $backup | Out-Null
$profileHashes = @{}
foreach ($file in @('package.json','pnpm-lock.yaml','pnpm-workspace.yaml','cordis.patch.yml')) {
    $source = Join-Path $profileDirectory $file
    if (Test-Path -LiteralPath $source -PathType Leaf) {
        $profileHashes[$file] = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
        Copy-Item -LiteralPath $source -Destination (Join-Path $backup $file)
        Assert-Hash (Join-Path $backup $file) $profileHashes[$file]
    }
}
foreach ($name in $nativeHashes.Keys) {
    $current = Join-Path $InstallDirectory $name
    Assert-Hash $current $oldHashes[$name]
    Copy-Item -LiteralPath $current -Destination (Join-Path $backup $name)
    Assert-Hash (Join-Path $backup $name) $oldHashes[$name]
}
Write-AtomicText (Join-Path $backup 'activation.json') ([ordered]@{ oldNativeHashes = $oldHashes; newNativeHashes = $nativeHashes; pluginArchiveHash = $archiveHash; profileHashes = $profileHashes } | ConvertTo-Json -Depth 5)
Write-Host "Backup: $backup"
$copied = [Collections.Generic.List[string]]::new()
$patchWritten = $false
try {
    Assert-Offline
    $env:DSH_HOME = $DshHome
    $pushed = $false
    try {
        Push-Location -LiteralPath $DshCheckout; $pushed = $true
        & $pnpm dsh plugin --profile web add $archive --ignore-scripts
        $installExit = $LASTEXITCODE
    } finally { if ($pushed) { Pop-Location } }
    if ($installExit -ne 0) { throw "DSH plugin installation failed with exit $installExit. See the CLI diagnostic; do not start DSH yet." }
    $installedPackage = Read-Json (Join-Path $profileDirectory 'node_modules\@ryanzhou416\dsh-plugin-aether-shell\package.json')
    if ($installedPackage['version'] -cne $packageVersion) { throw 'The installed plugin version is not the verified version.' }
    $afterManifest = Read-Json $manifestPath
    if ((Get-UnrelatedManifestJson $afterManifest) -cne $beforeUnrelated) { throw 'Profile manifest fields outside the AetherShell dependency changed; inspect the backup before continuing.' }
    $managerChanges = @('pnpm-lock.yaml','pnpm-workspace.yaml' | Where-Object {
        $file = Join-Path $profileDirectory $_
        (Test-Path -LiteralPath $file) -and (-not $profileHashes.ContainsKey($_) -or (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ne $profileHashes[$_])
    })
    Write-Host ("Package-manager files changed ($($managerChanges.Count)): " + ($managerChanges -join ', '))
    Assert-Offline
    foreach ($name in $nativeHashes.Keys) {
        Assert-Hash (Join-Path $NativeBuild $name) $nativeHashes[$name]
        Assert-Hash (Join-Path $InstallDirectory $name) $oldHashes[$name]
        $copied.Add($name)
        Copy-Item -LiteralPath (Join-Path $NativeBuild $name) -Destination (Join-Path $InstallDirectory $name) -Force
        Assert-Hash (Join-Path $InstallDirectory $name) $nativeHashes[$name]
    }
    Assert-TypedCapabilities (Join-Path $InstallDirectory 'ae.exe')
    if ([IO.File]::ReadAllText($patchPath) -cne $originalPatch) { throw 'Profile overrides changed during installation; no automatic overwrite is safe.' }
    $patchWritten = $true
    Write-AtomicText $patchPath $updatedPatch
    foreach ($name in $nativeHashes.Keys) { Assert-Hash (Join-Path $InstallDirectory $name) $nativeHashes[$name] }
    [ordered]@{ success = $true; nativeVersion = '12.0.2+ryan.3'; pluginVersion = $packageVersion; structuredDataMode = 'typed-v1'; backup = $backup; hostStarted = $false } | ConvertTo-Json
    Write-Host 'Activation files are ready. Start DSH using your normal launcher; this script did not start it.'
} catch {
    $failure = $_
    try {
        if ($patchWritten -and [IO.File]::ReadAllText($patchPath) -ceq $updatedPatch) { Write-AtomicText $patchPath $originalPatch }
    } catch { Write-Warning "Override rollback needs manual attention; backup: $backup; $($_.Exception.Message)" }
    foreach ($name in $copied) {
        try { Copy-Item -LiteralPath (Join-Path $backup $name) -Destination (Join-Path $InstallDirectory $name) -Force; Assert-Hash (Join-Path $InstallDirectory $name) $oldHashes[$name] }
        catch { Write-Warning "Native rollback needs manual attention: $name; backup: $backup; $($_.Exception.Message)" }
    }
    Write-Warning "Activation did not complete. Keep DSH stopped and inspect $backup. Plugin installation may already have succeeded; profile backups are not restored wholesale."
    throw $failure
} finally {
    if ($hadDshHome) { $env:DSH_HOME = $oldDshHome } else { Remove-Item Env:DSH_HOME -ErrorAction SilentlyContinue }
}
