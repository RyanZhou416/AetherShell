#Requires -Version 7.0
# Offline-activation controls and private atomic-write fixtures; no installer or child process runs.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$source = Join-Path $PSScriptRoot 'activate-dsh-aether.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($source, [ref]$tokens, [ref]$errors)
if ($errors.Count -gt 0) { throw 'The activation script does not parse.' }
foreach ($name in @('Get-UpdatedPatch','Get-CanonicalJson','Get-UnrelatedManifestJson','Get-Blockers','Write-AtomicText')) {
    $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if ($null -eq $definition) { throw "Required function missing: $name" }
    Invoke-Expression $definition.Extent.Text
}
$InstallDirectory = 'C:\Program Files\AetherShell'
$packageName = '@ryanzhou416/dsh-plugin-aether-shell'
$checks = 0
function Assert-Equal([string] $Name, [string] $Actual, [string] $Expected) {
    if ($Actual -cne $Expected) { throw "FAILED $Name" }
    $script:checks++
}
function Assert-Refused([string] $Name, [string] $Text) {
    $refused = $false
    try { $null = Get-UpdatedPatch $Text } catch { $refused = $true }
    if (-not $refused) { throw "FAILED ${Name}: unsafe layout was accepted" }
    $script:checks++
}
$prefix = "# preserve this comment`n- id: unrelated`n  config:`n    value: keep`n"
$block = "- id: aether-shell`n  config:`n    executable: 'C:\Program Files\AetherShell\ae.exe'`n    maxSessionsPerOwner: 4`n"
$suffix = "- id: another-plugin`n  config:`n    enabled: true`n"
$expectedBlock = $block.Replace("    maxSessionsPerOwner: 4", "    structuredDataMode: typed-v1`n    maxSessionsPerOwner: 4")
Assert-Equal 'insert mode and preserve surrounding entries' (Get-UpdatedPatch ($prefix + $block + $suffix)) ($prefix + $expectedBlock + $suffix)
Assert-Equal 'preserve CRLF' (Get-UpdatedPatch (($prefix + $block + $suffix).Replace("`n", "`r`n"))) (($prefix + $expectedBlock + $suffix).Replace("`n", "`r`n"))
$legacy = $expectedBlock.Replace('typed-v1', "'legacy'")
Assert-Equal 'replace only legacy setting' (Get-UpdatedPatch ($prefix + $legacy + $suffix)) ($prefix + $expectedBlock + $suffix)
Assert-Equal 'already typed is idempotent' (Get-UpdatedPatch ($prefix + $expectedBlock + $suffix)) ($prefix + $expectedBlock + $suffix)
Assert-Refused 'duplicate target' ($block + $block)
Assert-Refused 'wrong interpreter' ($block.Replace('AetherShell\ae.exe', 'OtherShell\ae.exe'))
Assert-Refused 'computed mode' ($expectedBlock.Replace('typed-v1', '!!js pickMode()'))
Assert-Refused 'duplicate mode' ($expectedBlock.Replace('    maxSessionsPerOwner: 4', '    structuredDataMode: legacy'))
Assert-Refused 'missing config' ($block.Replace("  config:`n", ''))
Assert-Refused 'missing target' $prefix
$before = @{ name='web'; dependencies=@{ $packageName='old'; other='same' }; scripts=@{ start='keep' }; dsh=@{profile=@{bundles=@('base',$packageName)}} }
$after = $before | ConvertTo-Json -Depth 10 | ConvertFrom-Json -AsHashtable
$after['dependencies'][$packageName] = 'new'
Assert-Equal 'ignore only target dependency spec' (Get-UnrelatedManifestJson $after) (Get-UnrelatedManifestJson $before)
$after['scripts']['start'] = 'changed'
if ((Get-UnrelatedManifestJson $after) -ceq (Get-UnrelatedManifestJson $before)) { throw 'FAILED unrelated script change was hidden' }
$checks++
$after['scripts']['start'] = 'keep'; $after['dependencies']['added-other'] = 'new'
if ((Get-UnrelatedManifestJson $after) -ceq (Get-UnrelatedManifestJson $before)) { throw 'FAILED unrelated added dependency was hidden' }
$checks++
$ExpectedHostProcessId = $PID
$WebPort = 3080
try {
    function Get-CimInstance { param($Namespace, $ClassName, $Filter, $ErrorAction) throw 'fixture ownership probe failure' }
    $blockers = @(Get-Blockers)
    if ($blockers.Count -ne 3 -or @($blockers | Where-Object { $_ -match 'Cannot verify' }).Count -ne 2) { throw 'FAILED ownership-probe errors did not fail closed' }
    $checks++
} finally { Remove-Item Function:Get-CimInstance }
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('aether-activation-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $target = Join-Path $temporary 'private-config.yml'
    Write-AtomicText $target "original`r`n中文"
    Write-AtomicText $target "updated`r`n中文😀"
    Assert-Equal 'atomic replacement preserves Unicode and CRLF' ([IO.File]::ReadAllText($target)) "updated`r`n中文😀"
    if ($IsWindows) {
        $held = [IO.File]::Open($target, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $failed = $false
        try { try { Write-AtomicText $target 'must not replace the locked file' } catch { $failed = $true } }
        finally { $held.Dispose() }
        if (-not $failed) { throw 'FAILED locked target did not reject replacement' }
        Assert-Equal 'failed replacement leaves original intact' ([IO.File]::ReadAllText($target)) "updated`r`n中文😀"
    }
    if (@(Get-ChildItem -LiteralPath $temporary -Filter '.aether-activation-*.tmp' -Force).Count -ne 0) { throw 'FAILED atomic writer left temporary files' }
    $checks++
} finally { Remove-Item -LiteralPath $temporary -Recurse -Force }
Write-Output "OFFLINE_ACTIVATION_TESTS=$checks passed; installer not executed; only owned temporary fixtures written and removed"
