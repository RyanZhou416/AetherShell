#Requires -Version 7.0
<#
Native interoperability smoke for the DSH AetherShell plugin against a real `ae`
executable. Standalone: no DSH checkout, no Node, no network. It exercises the
three transports the plugin drives:

  * `ae --deterministic -c` - one canonical JSON value on stdout; a failing
    program exits non-zero and keeps its diagnostic on stderr.
  * `ae agent execute` - one pretty-printed envelope on stdout, where an
    interpreter-level failure still exits 0, so exit status and `success` are
    independent facts.
  * `ae agent interactive` - one JSON request per stdin line over one process
    whose native state every request shares. Responses are multi-line JSON framed
    by brace balancing, and each reply is correlated by a nonce from a
    `try`/`catch` record, because the evaluated program shares that stdout. The
    session id is read from the reply, never computed.

Exit codes: 0 all passed, 1 a check failed, 2 setup or fatal. Progress goes to
stderr; stdout carries only the JSON summary.
#>
[CmdletBinding()]
param(
    [string] $Executable = (Join-Path -Path $PSScriptRoot -ChildPath '..\target\release\ae.exe'),
    [ValidateRange(1000, 600000)][int] $TimeoutMs = 60000
)

$ErrorActionPreference = 'Stop'

$script:Checks = [System.Collections.Generic.List[object]]::new()
$script:Procs = [System.Collections.Generic.List[object]]::new()
$script:Temp = $null
$script:Exe = $null
$script:Isolation = [ordered]@{}

function Add-Result {
    param([string] $Id, [string] $Name, [bool] $Passed, [string] $Detail = '')
    $script:Checks.Add([pscustomobject]@{ id = $Id; name = $Name; passed = $Passed; detail = $Detail })
    $tag = if ($Passed) { 'PASS' } else { 'FAIL' }
    $note = if ([string]::IsNullOrEmpty($Detail)) { '' } else { " :: $Detail" }
    [Console]::Error.WriteLine("[$tag] $Id $Name$note")
}

function Start-Ae {
    # The AetherShell control namespace is cleared before this script's explicit
    # values are applied, so an inherited mode, policy or workspace cannot change
    # what the run measures. ArgumentList makes each argument opaque.
    param([string[]] $Arguments)
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $script:Exe
    $startInfo.WorkingDirectory = $script:Temp
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $startInfo.StandardInputEncoding = $utf8
    $startInfo.StandardOutputEncoding = $utf8
    $startInfo.StandardErrorEncoding = $utf8
    foreach ($argument in $Arguments) { [void]$startInfo.ArgumentList.Add($argument) }
    foreach ($key in @($startInfo.Environment.Keys)) {
        if ($key -match '^(AETHER|AE)_') { [void]$startInfo.Environment.Remove($key) }
    }
    foreach ($entry in $script:Isolation.GetEnumerator()) {
        $startInfo.Environment[$entry.Key] = [string]$entry.Value
    }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $script:Procs.Add($process)
    return $process
}

function Invoke-Ae {
    # Both streams are drained before the wait: a child that fills a pipe while
    # the parent waits for exit would otherwise deadlock.
    param([string[]] $Arguments, [string] $Stdin = $null)
    $process = Start-Ae -Arguments $Arguments
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    if ($null -ne $Stdin) { $process.StandardInput.Write($Stdin) }
    $process.StandardInput.Close()
    if (-not $process.WaitForExit($TimeoutMs)) {
        try { $process.Kill($true) } catch { }
        [void]$process.WaitForExit(10000)
        # Settle the abandoned reads, then observe them, so a killed pipe cannot
        # surface later as an unobserved task exception.
        [void]$stdoutTask.Wait(5000)
        [void]$stderrTask.Wait(5000)
        try { [void]$stdoutTask.Exception; [void]$stderrTask.Exception } catch { }
        return [pscustomobject]@{ TimedOut = $true; ExitCode = $null; Out = ''; Err = '' }
    }
    [void]$stdoutTask.Wait(10000)
    [void]$stderrTask.Wait(10000)
    return [pscustomobject]@{ TimedOut = $false; ExitCode = $process.ExitCode; Out = $stdoutTask.Result; Err = $stderrTask.Result }
}

function ConvertTo-AeLiteral {
    # One mapping per character, so no produced escape is reprocessed. `$` becomes
    # `${'$'}`: left bare, a following `{` would start an interpolation in the
    # outer program that embeds this literal, consuming a caller's `${...}`
    # before the inner program ever sees it.
    param([AllowEmptyString()][string] $Text)
    $escapes = @{ 92 = '\\'; 34 = '\"'; 13 = '\r'; 10 = '\n'; 9 = '\t'; 36 = '${''$''}' }
    $builder = [System.Text.StringBuilder]::new()
    [void]$builder.Append('"')
    foreach ($character in $Text.ToCharArray()) {
        if ($escapes.ContainsKey([int]$character)) { [void]$builder.Append($escapes[[int]$character]) }
        else { [void]$builder.Append($character) }
    }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Get-JsonObjectEnd {
    # Index of the brace returning depth to zero, or -1 while incomplete. Strings
    # and escapes are tracked so a brace inside a value is not read as structure.
    param([AllowEmptyString()][string] $Text)
    $start = $Text.IndexOf('{')
    if ($start -lt 0) { return -1 }
    $depth = 0; $inString = $false; $escaped = $false
    for ($index = $start; $index -lt $Text.Length; $index++) {
        $character = $Text[$index]
        if ($inString) {
            if ($escaped) { $escaped = $false }
            elseif ($character -eq [char]92) { $escaped = $true }
            elseif ($character -eq [char]34) { $inString = $false }
            continue
        }
        if ($character -eq [char]34) { $inString = $true }
        elseif ($character -eq [char]123) { $depth++ }
        elseif ($character -eq [char]125) { $depth--; if ($depth -le 0) { return $index } }
    }
    return -1
}

function Read-NonceReply {
    # A document without the nonce is stream noise, not this request's reply: only
    # the nonce proves which document answers the request. The wait is on the
    # pending read, not a timer. A timed-out read stays pending on the state so
    # teardown can observe it instead of leaving an unobserved task exception.
    param($Session, [string] $Nonce)
    $deadline = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() + $TimeoutMs
    $buffer = [char[]]::new(4096)
    while ($true) {
        $end = Get-JsonObjectEnd -Text $Session.Output.Buffer
        if ($end -ge 0) {
            $start = $Session.Output.Buffer.IndexOf('{')
            $slice = $Session.Output.Buffer.Substring($start, $end - $start + 1)
            $Session.Output.Buffer = $Session.Output.Buffer.Substring($end + 1)
            $document = $null
            try { $document = $slice | ConvertFrom-Json -ErrorAction Stop } catch { $document = $null }
            if ($null -ne $document -and $null -ne $document.result -and ([string]$document.result.nonce) -ceq $Nonce) {
                return $document
            }
            continue
        }
        $remaining = $deadline - [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        if ($remaining -le 0) { return $null }
        $task = $Session.Output.Reader.ReadAsync($buffer, 0, $buffer.Length)
        $Session.Output.Pending = $task
        if (-not $task.Wait([int][Math]::Min(30000, $remaining))) { return $null }
        $Session.Output.Pending = $null
        $count = 0
        try { $count = $task.GetAwaiter().GetResult() } catch { return $null }
        if ($count -le 0) { return $null }
        $Session.Output.Buffer += [string]::new($buffer, 0, $count)
    }
}

function Invoke-SessionEval {
    # The wrapper turns an inner failure into a correlated record instead of a
    # lost reply, which is what the plugin's session tools decode.
    param($Session, [string] $Expression)
    $nonce = [guid]::NewGuid().ToString('N').Substring(0, 12)
    $literal = ConvertTo-AeLiteral -Text $nonce
    $code = "try { {nonce: $literal, ok: true, value: $Expression} } catch e { {nonce: $literal, ok: false, error: e} }"
    $json = @{ action = 'eval'; code = $code } | ConvertTo-Json -Depth 16 -Compress
    $Session.Process.StandardInput.Write($json + "`n")
    $Session.Process.StandardInput.Flush()
    return Read-NonceReply -Session $Session -Nonce $nonce
}

function New-SessionStep {
    param([string] $SessionId, [string] $UserCode)
    return 'sess_eval(' + (ConvertTo-AeLiteral -Text $SessionId) + ', ' + (ConvertTo-AeLiteral -Text $UserCode) + ')'
}

function Stop-All {
    foreach ($process in $script:Procs) {
        try { if (-not $process.HasExited) { $process.Kill($true); [void]$process.WaitForExit(10000) } } catch { }
        try { $process.StandardInput.Dispose() } catch { }
        try { $process.Dispose() } catch { }
    }
    $script:Procs.Clear()
}

function Remove-PrivateTemp {
    # Removes only the directory this script created: an ordinary directory under
    # the system temp path, never a reparse point that could redirect the delete.
    if ($null -eq $script:Temp -or -not (Test-Path -LiteralPath $script:Temp)) { return }
    try {
        $item = Get-Item -LiteralPath $script:Temp -Force
        if ($item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -and
            $item.Name.StartsWith('ae-dsh-compat-') -and
            $item.Parent.FullName.TrimEnd('\') -eq ([System.IO.Path]::GetTempPath()).TrimEnd('\')) {
            Remove-Item -LiteralPath $script:Temp -Recurse -Force
        } else {
            [Console]::Error.WriteLine("[WARN] refused to remove unexpected path: $($script:Temp)")
        }
    } catch { [Console]::Error.WriteLine("[WARN] could not remove $($script:Temp): $($_.Exception.Message)") }
}

# --- Setup ------------------------------------------------------------------

$exitCode = 2
$fatal = $null
try {
    $script:Exe = [System.IO.Path]::GetFullPath($Executable)
    if (-not (Test-Path -LiteralPath $script:Exe -PathType Leaf)) {
        $fatal = "executable not found: $($script:Exe) - build it first (cargo build --release)"
    } else {
        $script:Temp = Join-Path ([System.IO.Path]::GetTempPath()) ('ae-dsh-compat-' + [guid]::NewGuid().ToString('N'))
        $null = New-Item -ItemType Directory -Path $script:Temp -Force
        $root = Join-Path $script:Temp 'iso'
        foreach ($name in @('config', 'data', 'cache')) { $null = New-Item -ItemType Directory -Path (Join-Path $root $name) -Force }
        $script:Isolation = [ordered]@{
            'AETHER_MODE' = 'human'; 'NO_COLOR' = '1'; 'AETHER_CONFIG' = (Join-Path $root 'config.toml')
            'XDG_CONFIG_HOME' = (Join-Path $root 'config'); 'XDG_DATA_HOME' = (Join-Path $root 'data'); 'XDG_CACHE_HOME' = (Join-Path $root 'cache')
        }
    }
} catch { $fatal = "setup failed: $($_.Exception.Message)" }

if ($null -ne $fatal) {
    [ordered]@{ schemaVersion = 1; ok = $false; setupFailure = $fatal; checks = @() } | ConvertTo-Json -Depth 6
    Stop-All
    Remove-PrivateTemp
    exit 2
}

$exeItem = Get-Item -LiteralPath $script:Exe
$version = (Invoke-Ae -Arguments @('--version')).Out.Trim()

try {
    [Console]::Error.WriteLine("ae = $($version)  ($($exeItem.Length) bytes, modified $($exeItem.LastWriteTime.ToString('u')))")
    [Console]::Error.WriteLine('')
    [Console]::Error.WriteLine('--- ae --deterministic -c ---')

    $expressions = @(
        @{ Id = 'A1'; Name = 'filter-map-typed-array'; Code = '[1, 2, 3, 4] | where(fn(x) => x > 2) | map(fn(x) => x * 10)'; Want = '[30,40]' },
        @{ Id = 'A2'; Name = 'sort-ascending'; Code = '[3, 1, 2] | sort'; Want = '[1,2,3]' },
        @{ Id = 'A3'; Name = 'sort-reverse-flag'; Code = '[3, 1, 2] | sort("-r")'; Want = '[3,2,1]' },
        @{ Id = 'A4'; Name = 'json-null-roundtrip-nested'; Code = 'json_parse(json_stringify({a: None, nested: [None]}))'; Want = '{"a":null,"nested":[null]}' },
        @{ Id = 'A7'; Name = 'raw-path-and-template'; Code = 'r"C:\temp\new ${1+2}"'; Want = '"C:\\temp\\new ${1+2}"' }
    )
    foreach ($expression in $expressions) {
        $run = Invoke-Ae -Arguments @('--deterministic', '-c', $expression.Code)
        if ($run.TimedOut) { Add-Result $expression.Id $expression.Name $false "timed out after ${TimeoutMs}ms"; continue }
        $actual = $run.Out.Trim()
        $passed = ($run.ExitCode -eq 0) -and ($actual -ceq $expression.Want)
        Add-Result $expression.Id $expression.Name $passed $(if ($run.ExitCode -eq 0) { $actual } else { "exit $($run.ExitCode); $($run.Err.Trim())" })
    }

    # A fixed negative: a failing program exits non-zero on stderr rather than
    # emitting something that reads as a value.
    $refusal = Invoke-Ae -Arguments @('--deterministic', '-c', 'no_such_builtin_xyz()')
    Add-Result 'A5' 'failing-program-exits-nonzero' `
        ((-not $refusal.TimedOut) -and ($refusal.ExitCode -ne 0) -and ($refusal.Err -match 'unknown builtin')) "exit=$($refusal.ExitCode)"

    # `sort` reads only the reverse flags, so a comparator written by habit must be
    # refused with a coded error the caller can branch on, not silently ignored.
    $comparator = Invoke-Ae -Arguments @('--deterministic', '-c', '[3, 1, 2] | sort(fn(a, b) => a - b)')
    Add-Result 'A6' 'sort-refuses-comparator' `
        ((-not $comparator.TimedOut) -and ($comparator.ExitCode -ne 0) -and ($comparator.Err -match 'E_BAD_ARG')) "exit=$($comparator.ExitCode)"

    [Console]::Error.WriteLine('')
    foreach ($invalid in @(
        @{ Id = 'A8'; Name = 'unknown-escape-is-a-parse-error'; Code = '"\q"' },
        @{ Id = 'A9'; Name = 'unclosed-string-is-a-parse-error'; Code = '"abc' }
    )) {
        $run = Invoke-Ae -Arguments @('--deterministic', '-c', $invalid.Code)
        Add-Result $invalid.Id $invalid.Name ((-not $run.TimedOut) -and ($run.ExitCode -ne 0) -and ($run.Err -match 'E_PARSE')) "exit=$($run.ExitCode)"
    }
    [Console]::Error.WriteLine('--- ae agent execute ---')

    $requests = @(
        @{ Id = 'B1'; Name = 'call-len'; RequestText = '{"action":"call","builtin":"len","args":[[1,2,3]]}' },
        @{ Id = 'B2'; Name = 'pipeline-where-select'; RequestText = '{"action":"pipeline","input":[{"size":1},{"size":5},{"size":9}],"steps":[{"builtin":"where","predicate":"size > 2","select":"size"}]}' },
        @{ Id = 'B3'; Name = 'ontology-manifest'; RequestText = '{"action":"call","builtin":"ontology_manifest","args":[]}' },
        @{ Id = 'B4'; Name = 'typed-data-capabilities'; RequestText = '{"action":"capabilities"}'; Expected = '{"protocol":"aethershell-agent-api","protocol_version":1,"features":{"typed_arguments_v1":true,"typed_pipeline_input_v1":true,"explicit_null_pipeline_input_v1":true}}' },
        @{ Id = 'B5'; Name = 'typed-record-literals'; RequestText = '{"action":"call","builtin":"first","args":[[{"a-b":"literal ${1+2}","中文":null}]]}'; Expected = '{"a-b":"literal ${1+2}","中文":null}' },
        @{ Id = 'B6'; Name = 'typed-null-seed'; RequestText = '{"action":"pipeline","input":null,"steps":[{"builtin":"type_of"}]}'; Expected = '"Null"' },
        @{ Id = 'B7'; Name = 'unicode-scalar-length'; RequestText = '{"action":"call","builtin":"char_len","args":["你好😀"]}'; Expected = '3' },
        @{ Id = 'B8'; Name = 'explicit-process-runner'; RequestText = (@{action='call'; builtin='proc_run'; args=@(,@($script:Exe, '--version'))} | ConvertTo-Json -Compress -Depth 5) }
    )
    foreach ($request in $requests) {
        $run = Invoke-Ae -Arguments @('agent', 'execute') -Stdin $request.RequestText
        $document = $null
        try { $document = $run.Out | ConvertFrom-Json -ErrorAction Stop } catch { $document = $null }
        $matches = $true
        if ($request.Id -eq 'B4' -and $null -ne $document) {
            $cap = $document.result
            $matches = ($cap.protocol -ceq 'aethershell-agent-api') -and ($cap.protocol_version -eq 1) -and
                ($cap.features.typed_arguments_v1 -ceq $true) -and ($cap.features.typed_pipeline_input_v1 -ceq $true) -and
                ($cap.features.explicit_null_pipeline_input_v1 -ceq $true)
        } elseif ($request.Id -eq 'B8' -and $null -ne $document) {
            $captured = $document.result
            $matches = ($captured.success -ceq $true) -and ($captured.process.exit_code -eq 0) -and
                ($captured.process.cleanup_complete -ceq $true) -and ($captured.process.group_termination_requested -ceq $true) -and
                ($captured.stdout.decoded -ceq $true) -and ($captured.stdout.complete -ceq $true) -and
                ($captured.stdout.text.Trim() -ceq $version) -and
                ($captured.stdout.base64 -ceq [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($captured.stdout.text)))
        } elseif ($request.ContainsKey('Expected') -and $null -ne $document) {
            $actual = ConvertTo-Json -InputObject $document.result -Compress -Depth 20
            $normalized = ConvertTo-Json -InputObject ($request.Expected | ConvertFrom-Json) -Compress -Depth 20
            $matches = $actual -ceq $normalized
        }
        Add-Result $request.Id $request.Name (($null -ne $document) -and ($document.success -eq $true) -and ($run.ExitCode -eq 0) -and $matches) `
            "success=$($document.success) exit=$($run.ExitCode) expectedValueMatches=$matches"
    }

    [Console]::Error.WriteLine('')
    [Console]::Error.WriteLine('--- ae agent interactive (one process) ---')

    $session = $null
    try {
        $process = Start-Ae -Arguments @('agent', 'interactive')
        $session = [pscustomobject]@{
            Process = $process
            Output  = [pscustomobject]@{ Reader = $process.StandardOutput; Buffer = ''; Pending = $null }
        }

        # The id comes out of the reply; liveness is proven by the state the
        # following checks read back, not by decoding the id.
        $reply = Invoke-SessionEval -Session $session -Expression 'sess_open()'
        $sessionId = if ($null -ne $reply) { [string]$reply.result.value.session } else { '' }
        Add-Result 'S1' 'session-open-returns-live-id' `
            (($null -ne $reply) -and ($reply.result.ok -eq $true) -and ($sessionId.Length -gt 0)) "session=$sessionId pid=$($process.Id)"

        if ($sessionId.Length -gt 0) {
            $null = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId 'let greeting = "你好 ✓"')
            $read = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId 'greeting')
            Add-Result 'S2' 'session-variable-retained-unicode' `
                (($null -ne $read) -and ($read.result.ok -eq $true) -and ([string]$read.result.value -ceq '你好 ✓')) "value=$($read.result.value)"

            $null = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId 'let double = fn(n) => n * 2')
            $call = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId 'double(21)')
            Add-Result 'S3' 'session-lambda-retained' `
                (($null -ne $call) -and ($call.result.ok -eq $true) -and ($call.result.value -eq 42)) "value=$($call.result.value)"

            # The encoder is load-bearing: this program holds a literal `$` and a
            # `${...}` interpolation, so it survives only if the encoding stops
            # the outer parse from consuming the interpolation.
            $userCode = 'let price = "$5"; let who = "agent"; "${who} pays ${price}"'
            $encoded = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId $userCode)
            Add-Result 'S4' 'session-encoded-dollar-interpolation' `
                (($null -ne $encoded) -and ($encoded.result.ok -eq $true) -and ([string]$encoded.result.value -ceq 'agent pays $5')) "value=$($encoded.result.value)"

            $closed = Invoke-SessionEval -Session $session -Expression ('sess_close(' + (ConvertTo-AeLiteral -Text $sessionId) + ')')
            $reused = Invoke-SessionEval -Session $session -Expression (New-SessionStep $sessionId '1')
            Add-Result 'S5' 'session-close-discards-environment' `
                (($null -ne $closed) -and ($closed.result.value.closed -eq $true) -and ($null -ne $reused) -and ($reused.result.ok -eq $false)) `
                "closed=$($closed.result.value.closed) reuseRefused=$($null -ne $reused -and $reused.result.ok -eq $false)"
        }
    } catch {
        Add-Result 'S0' 'session-surface-completed' $false $_.Exception.Message
    } finally {
        if ($null -ne $session) {
            try {
                if (-not $session.Process.HasExited) { $session.Process.Kill($true); [void]$session.Process.WaitForExit(10000) }
            } catch { }
            try { if ($null -ne $session.Output.Pending) { $null = $session.Output.Pending.Exception } } catch { }
            Add-Result 'S6' 'session-process-stopped-and-awaited' $session.Process.HasExited "pid=$($session.Process.Id) stopped=$($session.Process.HasExited)"
        }
    }

    $expectedIds = @('A1','A2','A3','A4','A5','A6','A7','A8','A9','B1','B2','B3','B4','B5','B6','B7','B8','S1','S2','S3','S4','S5','S6')
    $actualIds = @($script:Checks | ForEach-Object { $_.id })
    if ($actualIds.Count -ne $expectedIds.Count -or @(Compare-Object $expectedIds $actualIds).Count -ne 0) {
        Add-Result 'Z0' 'all-required-checks-ran' $false "expected $($expectedIds.Count) checks; observed $($actualIds.Count)"
    }
    $passed = @($script:Checks | Where-Object { $_.passed }).Count
    $failed = @($script:Checks | Where-Object { -not $_.passed }).Count
    $exitCode = if ($failed -eq 0) { 0 } else { 1 }
    [Console]::Error.WriteLine('')
    [Console]::Error.WriteLine("=== $passed passed, $failed failed ===")

    [ordered]@{
        schemaVersion = 1; ok = ($failed -eq 0); executable = $script:Exe
        executableSha256 = (Get-FileHash -LiteralPath $script:Exe -Algorithm SHA256).Hash
        executableModifiedUtc = $exeItem.LastWriteTime.ToUniversalTime().ToString('o')
        version = $version; isolation = $script:Isolation
        checks = @($script:Checks); passed = $passed; failed = $failed
    } | ConvertTo-Json -Depth 6
} catch {
    [Console]::Error.WriteLine("[FATAL] $($_.Exception.Message)")
    [ordered]@{ schemaVersion = 1; ok = $false; fatal = $_.Exception.Message; checks = @($script:Checks) } | ConvertTo-Json -Depth 6
    $exitCode = 2
} finally {
    Stop-All
    Remove-PrivateTemp
}

exit $exitCode
