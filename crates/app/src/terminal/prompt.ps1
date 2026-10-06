# Owned terminal bootstrap: no user profile, no execution-policy change, no evaluation of prompt text.
New-Variable -Name WesPromptGit -Value $env:WES_PROMPT_GIT -Option Constant -Scope Global
New-Variable -Name WesPromptEnvironment -Value $env:WES_PROMPT_ENVIRONMENT -Option Constant -Scope Global
New-Variable -Name WesHistory -Option Constant -Scope Global -Value @{
    File = $env:WES_HISTORY_FILE
    Bytes = [int64]$env:WES_HISTORY_BYTES
    Entries = New-Object System.Collections.Generic.List[string]
    Last = [int64]-1
    Accepting = $false
    Restoring = New-Object System.Collections.Generic.List[string]
    Restored = 0
    Observed = [int64]-2
    Line = ''
}
Remove-Item Env:WES_BOOTSTRAP, Env:WES_PROMPT_GIT, Env:WES_PROMPT_ENVIRONMENT, Env:WES_HISTORY_FILE, Env:WES_HISTORY_BYTES -ErrorAction SilentlyContinue

# A host started with Ctrl+C disabled passes that setting to every process below it, and the
# console then delivers no interrupt to this shell or to what it runs. Accept it again here.
try {
    $assembly = [AppDomain]::CurrentDomain.DefineDynamicAssembly((New-Object Reflection.AssemblyName 'WesConsole'), [Reflection.Emit.AssemblyBuilderAccess]::Run)
    $type = $assembly.DefineDynamicModule('WesConsole').DefineType('WesConsole.Native', 'Public,Class')
    $method = $type.DefinePInvokeMethod('SetConsoleCtrlHandler', 'kernel32.dll', 'Public,Static,PinvokeImpl', 'Standard', [bool], @([IntPtr], [bool]), 'Winapi', 'Auto')
    $method.SetImplementationFlags('PreserveSig')
    [void]$type.CreateType()::SetConsoleCtrlHandler([IntPtr]::Zero, $false)
} catch {}
Remove-Variable assembly, type, method -ErrorAction SilentlyContinue

# The pane speaks UTF-8 in both directions, whatever code page the machine defaults to.
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
$global:OutputEncoding = New-Object System.Text.UTF8Encoding $false

# The system's own line editor, when the machine's policy admits it. This pane's commands never
# enter the user's own history file.
try {
    Import-Module PSReadLine -ErrorAction Stop
    Set-PSReadLineOption -HistorySaveStyle SaveNothing
} catch {}

# The pane's own history: private UTF-8 commands separated by NUL, never shell source.
if ($WesHistory.File) {
    try {
        $text = (New-Object System.Text.UTF8Encoding $false, $true).GetString([IO.File]::ReadAllBytes($WesHistory.File))
        foreach ($entry in $text.Split([char[]]@([char]0))) {
            if (-not $entry) { continue }
            $WesHistory.Entries.Add($entry)
        }
    } catch {}
    Remove-Variable text, entry -ErrorAction SilentlyContinue
}
# One complete snapshot replaces the file, so a reader never sees a partial write. No records
# is an empty file: a separator alone would be an empty record, which the pane refuses to open.
function global:Add-WesHistory([string]$command) {
    if (-not $WesHistory.File -or [string]::IsNullOrWhiteSpace($command) -or $command.Contains([string][char]0)) { return }
    $WesHistory.Entries.Add($command.Replace("`r`n", "`n"))
    $encoding = New-Object System.Text.UTF8Encoding $false
    $size = [int64]0
    foreach ($entry in $WesHistory.Entries) { $size += $encoding.GetByteCount($entry) + 1 }
    while ($WesHistory.Entries.Count -gt 1000 -or ($WesHistory.Bytes -gt 0 -and $size -gt $WesHistory.Bytes -and $WesHistory.Entries.Count -gt 0)) {
        $size -= $encoding.GetByteCount($WesHistory.Entries[0]) + 1
        $WesHistory.Entries.RemoveAt(0)
    }
    try {
        $next = $WesHistory.File + '.next'
        $records = [byte[]]@()
        if ($WesHistory.Entries.Count -gt 0) {
            $records = $encoding.GetBytes((($WesHistory.Entries -join [string][char]0) + [string][char]0))
        }
        [IO.File]::WriteAllBytes($next, $records)
        [IO.File]::Replace($next, $WesHistory.File, [NullString]::Value)
    } catch {}
}
# With the line editor a command is recorded when it is accepted, before it runs, so a pane
# closed during a long command still remembers it. The editor offers this handler the
# session's restored records as well, when it first reads them; those are already the pane's
# own and are recognised in order instead of being recorded a second time.
if (Get-Module PSReadLine) {
    try {
        Set-PSReadLineOption -AddToHistoryHandler {
            param([string]$line)
            $line = $line.Replace("`r`n", "`n")
            if ($WesHistory.Restored -lt $WesHistory.Restoring.Count -and $line -ceq $WesHistory.Restoring[$WesHistory.Restored]) {
                $WesHistory.Restored++
                return $true
            }
            $WesHistory.Restored = $WesHistory.Restoring.Count
            Add-WesHistory $line
            return $true
        }
        $WesHistory.Accepting = $true
    } catch {}
}
function global:Save-WesHistory {
    $last = Get-History -Count 1
    if (-not $last -or $last.Id -eq $WesHistory.Last) { return }
    $first = $WesHistory.Last -lt 0
    $previous = $WesHistory.Last
    $WesHistory.Last = $last.Id
    if ($first) {
        # The first prompt follows this bootstrap itself, which is not one of the pane's
        # commands. The session forgets it and takes the pane's records as text instead;
        # nothing here executes them. The line editor reads the session's history when it
        # first asks for a line, which is after this prompt.
        Clear-History
        $now = Get-Date
        foreach ($entry in $WesHistory.Entries) {
            $WesHistory.Restoring.Add($entry.Replace("`r`n", "`n"))
            Add-History -InputObject ([pscustomobject]@{
                CommandLine = $entry; ExecutionStatus = 'Completed'
                StartExecutionTime = $now; EndExecutionTime = $now
            })
        }
        $last = Get-History -Count 1
        if ($last) { $WesHistory.Last = $last.Id }
        if (-not $WesHistory.Accepting) { Watch-WesHistory }
        return
    }
    # Without the line editor a line that started no command was not observed; it is recorded
    # now that its prompt has returned.
    $observed = $WesHistory.Observed -eq $previous -and $WesHistory.Line -ceq $last.CommandLine.Replace("`r`n", "`n")
    if (-not $WesHistory.Accepting -and -not $observed) { Add-WesHistory $last.CommandLine }
}
# Without the line editor nothing announces an accepted line. The shell does announce each
# command it is about to look up for the line the user entered, and the line is then the
# outermost script running. Its first command records it, before that command runs, so a pane
# closed during a long command remembers it here as well. A line that starts no command, such
# as a bare expression or loop, is recorded when its prompt returns.
function global:Watch-WesHistory {
    try {
        $ExecutionContext.InvokeCommand.PreCommandLookupAction = {
            param([string]$name, $lookup)
            if ($lookup.CommandOrigin -ne 'Runspace') { return }
            try {
                $stack = @(Get-PSCallStack)
                if ($stack.Count -lt 2) { return }
                $line = $stack[-1].Position.StartScriptPosition.GetFullScript().Replace("`r`n", "`n")
                # The host asks for the prompt the same way; that is not the user's line.
                if ($line -ceq 'prompt' -and $name -ceq 'prompt') { return }
                if ($WesHistory.Observed -eq $WesHistory.Last -and $WesHistory.Line -ceq $line) { return }
                $WesHistory.Observed = $WesHistory.Last
                $WesHistory.Line = $line
                Add-WesHistory $line
            } catch {}
        }
    } catch {}
}

function global:prompt {
    $previous = $global:LASTEXITCODE
    Save-WesHistory
    $environment = 'no-env'
    if ($WesPromptEnvironment) {
        try {
            $line = ([IO.File]::ReadAllText($WesPromptEnvironment) -split "`n")[0].TrimEnd("`r")
            if ($line) { $environment = $line }
        } catch {}
    }
    $label = [Environment]::UserName + '@' + $environment
    $location = $ExecutionContext.SessionState.Path.CurrentLocation
    $directory = $location.ProviderPath
    $userHome = $env:USERPROFILE
    if ($userHome -and $directory -eq $userHome) {
        $directory = '~'
    } elseif ($userHome -and $directory.StartsWith($userHome + '\', [StringComparison]::OrdinalIgnoreCase)) {
        $directory = '~' + $directory.Substring($userHome.Length)
    }
    $branch = ''
    if ($WesPromptGit -and $location.Provider.Name -eq 'FileSystem') {
        $locks = $env:GIT_OPTIONAL_LOCKS
        $env:GIT_OPTIONAL_LOCKS = '0'
        try {
            $branch = & $WesPromptGit --no-pager -c core.fsmonitor=false symbolic-ref --quiet --short HEAD 2>$null
            if ($LASTEXITCODE -ne 0) {
                $commit = & $WesPromptGit --no-pager -c core.fsmonitor=false rev-parse --short HEAD 2>$null
                if ($LASTEXITCODE -eq 0) { $branch = '@' + $commit } else { $branch = '' }
            }
        } catch {
            $branch = ''
        } finally {
            $env:GIT_OPTIONAL_LOCKS = $locks
        }
    }
    # The labels are data: a control character in any of them is shown, never sent to the pane.
    $label = $label -replace '\p{Cc}', '?'
    $directory = $directory -replace '\p{Cc}', '?'
    $branch = "$branch" -replace '\p{Cc}', '?'
    Write-Host $label -NoNewline -ForegroundColor Green
    Write-Host ' ' -NoNewline
    Write-Host $directory -NoNewline -ForegroundColor Cyan
    if ($branch) {
        Write-Host ' (' -NoNewline
        Write-Host $branch -NoNewline -ForegroundColor Yellow
        Write-Host ')' -NoNewline
    }
    $global:LASTEXITCODE = $previous
    return ' > '
}
