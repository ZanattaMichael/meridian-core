<#
.SYNOPSIS
Runs a project the Meridian DSC plugin compiled through the real DscPipelineRunner, the way
the plugin's runner does, and checks what the runner reports at each step.

.DESCRIPTION
The plugin's runner (plugins/dsc/src/runner.rs) is a plan and an apply:

  plan   Invoke-DscRunner -Mode Test; a resource reported as FAIL is a change
  apply  the same Test, then Invoke-DscRunner -Mode Set; a FAIL then is a failure
         (with a DscPipelineRunner whose Set results carry Drifted, the plugin runs the
         Set alone; this script keeps the Test so it checks the drift either way)

This script makes the same calls, with the same arguments, over a project that was compiled
elsewhere, and asserts:

  1. the first Test reports exactly the resources named by -Drifted as not in their desired state,
  2. the Set that follows fails nothing,
  3. a second Test reports nothing left to change.

It exists so that the compiled output can be proven against real Windows DSC resources on a
Windows machine. It needs no Rust toolchain there.

.PARAMETER Project
The directory `meridian compile --out` wrote for the document: the one that holds Datum.yml.

.PARAMETER Drifted
The ids of the resources that are expected not to be in their desired state before the Set.

.PARAMETER Module
The DscPipelineRunner module to import: a name PowerShell can find, or the path of its manifest.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Project,

    [string[]]$Drifted = @(),

    [string]$Module = 'Dsc.PipelineRunner'
)

$ErrorActionPreference = 'Stop'
Import-Module $Module

$Project = (Resolve-Path -LiteralPath $Project).Path
if (-not (Test-Path -LiteralPath (Join-Path $Project 'Datum.yml'))) {
    throw "$Project holds no Datum.yml; it is not a project the DSC plugin compiled."
}

# One run, and every resource's result in it.
function Invoke-Run {
    param([string]$Mode)

    Write-Host "::group::Invoke-DscRunner -Mode $Mode"
    try {
        $run = Invoke-DscRunner -ConfigurationSourcePath $Project -Mode $Mode
    }
    finally {
        Write-Host '::endgroup::'
    }
    # The same serialisation the plugin reads, so that what it would see is on the log.
    Write-Host ('MERIDIAN-RESULT ' + ($run | ConvertTo-Json -Depth 8 -Compress))

    foreach ($configuration in @($run.Configurations)) {
        if ($configuration.Status -eq 'AbortedByException') {
            throw "DscPipelineRunner stopped during ${Mode}: $($configuration.ErrorMessage)"
        }
        foreach ($result in @($configuration.Results)) {
            $result
        }
    }
}

function Get-Failed {
    param($Results)
    @($Results | Where-Object { $_.Status -eq 'FAIL' } | ForEach-Object { $_.InstanceName } | Sort-Object)
}

# Plan: what has drifted.
$before = @(Invoke-Run -Mode Test)
if ($before.Count -eq 0) {
    throw 'The runner reported no resources at all.'
}
$drift = Get-Failed $before
$expected = @($Drifted | Sort-Object)
if (($drift -join ',') -ne ($expected -join ',')) {
    $messages = ($before | ForEach-Object { "  $($_.InstanceName): $($_.Status) $($_.ErrorMessage)" }) -join "`n"
    throw "Expected [$($expected -join ', ')] to have drifted, and the runner reported [$($drift -join ', ')].`n$messages"
}
Write-Host "Plan: [$($drift -join ', ')] would change."

# Apply: set them.
$set = @(Invoke-Run -Mode Set)
$failed = Get-Failed $set
if ($failed.Count -gt 0) {
    $messages = ($set | Where-Object { $_.Status -eq 'FAIL' } | ForEach-Object { "  $($_.InstanceName): $($_.ErrorMessage)" }) -join "`n"
    throw "The runner could not set [$($failed -join ', ')].`n$messages"
}
Write-Host 'Apply: every resource was set.'

# Plan again: nothing left.
$after = @(Invoke-Run -Mode Test)
$left = Get-Failed $after
if ($left.Count -gt 0) {
    $messages = ($after | Where-Object { $_.Status -eq 'FAIL' } | ForEach-Object { "  $($_.InstanceName): $($_.ErrorMessage)" }) -join "`n"
    throw "After the apply [$($left -join ', ')] are still not in their desired state.`n$messages"
}
Write-Host 'Plan: in sync.'
