# Activate the verified DSH AetherShell versions offline

This procedure activates native `12.0.2+ryan.3`, adapter `0.1.0-dsh017rc1.5`, and `structuredDataMode: typed-v1` in the existing Web profile. It does not launch a second server or start/restart DSH. The [activation script](../../scripts/activate-dsh-aether.ps1) pins the verified executable and tarball hashes; if a file was rebuilt or replaced, stop and verify the new artifact instead of bypassing the check.

## Prepared Windows launcher workflow

When an activation plan has been prepared for the existing Web profile, the operator only needs to stop the current DSH Host, run [build.cmd](<C:/Project/deepseek-harness/build.cmd>), and then run [run.cmd](<C:/Project/deepseek-harness/run.cmd>). The build remains the normal repository build. The run launcher checks and applies the prepared update before its normal Host launch; Windows may ask for permission to update Program Files. No separate PowerShell command is needed. Do not start DSH elsewhere or close the activation window midway through the update.

The one-shot plan pins this script's hash and the intended DSH home. Startup refuses a changed script, a still-running Host, an unsupported plan or an activation failure. On success the launcher records completion, so later runs do not reinstall the update. This preparation does not change the currently loaded plugin or interpreter.

## Manual maintenance fallback

The remaining steps are for maintainers running the installer separately, not required for the prepared build/run workflow.

### Before stopping DSH

Open an **external PowerShell 7 terminal as administrator**. Keep that terminal open throughout the procedure. Record the current GUI process so the installer can refuse while the old Host is still exiting:

```powershell
$hostProcessId = (Get-NetTCPConnection -LocalPort 3080 -State Listen | Select-Object -First 1).OwningProcess
pwsh -NoProfile -File "C:\Project\AetherShell\scripts\activate-dsh-aether.ps1" -CheckOnly -ExpectedHostProcessId $hostProcessId
```

`CheckOnly` verifies pinned artifacts and the target profile layout, and reports a running Host or an uncertain ownership probe as a blocker. It writes no files and starts no interpreter. Exit code `2` means blocked; `0` means no running-process blocker was found, not that installation occurred or elevation was granted. Actual offline activation performs a bounded capability probe before installing. Do not rebuild or repack the artifacts between preflight and activation.

## Stop, install, then start

1. Stop DSH normally using its existing terminal or launcher. Closing a browser tab alone does not stop the Host. Let it flush and exit; the script does not kill it or any interpreter.
2. In the same external administrator terminal, run:

```powershell
pwsh -NoProfile -File "C:\Project\AetherShell\scripts\activate-dsh-aether.ps1" -ExpectedHostProcessId $hostProcessId
```

3. Require exit code zero and the final `success: true` summary. It must name native `12.0.2+ryan.3`, plugin `0.1.0-dsh017rc1.5`, and mode `typed-v1`.
4. Start DSH using the usual launcher. This script never starts it. After reconnecting, ask the agent to verify actual tool calls rather than inferring activation from the restart alone.

The installer refuses a live Host process, a listener on the configured GUI port, an uncertain ownership probe, any running `ae.exe`/`aimodel.exe`, or an unverified installed native binary. It backs up the previous native executables and the profile manifest, lockfile and overrides under a unique directory below `C:\Project\deepseek-harness-data\backups`, including a hash receipt. It uses the official `pnpm dsh plugin --profile web add ... --ignore-scripts` command through the checkout's pnpm launcher and modifies only the AetherShell override's data mode. The whole profile manifest except the target package specification is checked after installation; changed package-manager filenames are reported. Subscription plugin versions are not upgraded by this procedure. An inherited `DSH_HOME` selecting another home requires an explicit `-DshHome` argument.

## Verification after reconnecting

These distinguish the active versions from the prior runtime and adapter:

- `aether_eval` with `r"C:\temp\new ${1+2}"` returns that literal text.
- `aether_eval` with `"\q"` reports `E_PARSE` rather than returning `q`.
- `aether_call` with builtin `first` and `args: [[{"a-b":"literal ${1+2}","中文":null}]]` returns the record unchanged rather than rejecting the key.
- `aether_pipeline` with `input: null` and `steps: [{"builtin":"type_of"}]` returns `Null`.
- `aether_discover("proc_run")` describes the explicit argv, encoding and raw-byte output interface.

Typed mode probes capabilities for each structured request. An unsupported interpreter fails before argument data is sent; there is no legacy fallback. Full numeric precision limits, file-policy refusals, deadlines and session ownership remain in effect.

## If activation fails

Keep DSH stopped. Preserve the reported backup directory and inspect the error. The script attempts to restore only native files it overwrote and its own override edit if that text has not changed again. A plugin installation may already have succeeded; it does not restore entire manifests or lockfiles over potentially unrelated work. Do not claim activation or restart into an uncertain combination. The retained adapter `.4` tarball and backed-up executables allow a deliberate rollback; a rollback to an older interpreter must also restore `structuredDataMode: legacy` or remove that field.
