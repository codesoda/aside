# Native release smoke test. GitHub-hosted Windows runners have disposable user profiles.
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Installer = Join-Path $Root 'install.ps1'
$Bin = Join-Path $HOME '.aside/bin'
$Aside = Join-Path $Bin 'aside.exe'
$Inspect = Join-Path $Bin 'inspect.exe'
$Skills = @('.claude', '.codex', '.agents') | ForEach-Object { Join-Path $HOME "$_/skills/aside" }

if ($HOME -ne $env:USERPROFILE) { throw 'Run this smoke test with the disposable runner user profile' }
foreach ($Path in @($Aside, $Inspect) + $Skills) {
    if (Test-Path -LiteralPath $Path) { throw "Refusing to overwrite existing runner data: $Path" }
}

$OriginalUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("aside-windows-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Temp | Out-Null
try {
    $InputFile = Join-Path $Temp 'stdin.txt'
    $OutputFile = Join-Path $Temp 'stdout.txt'
    $ErrorFile = Join-Path $Temp 'stderr.txt'
    [IO.File]::WriteAllText($InputFile, '')
    # Redirect stdin so the installer cannot prompt for or launch a live agent demo.
    $Process = Start-Process -FilePath 'pwsh.exe' -ArgumentList @('-NoProfile', '-NonInteractive', '-File', "`"$Installer`"") -Wait -PassThru -RedirectStandardInput $InputFile -RedirectStandardOutput $OutputFile -RedirectStandardError $ErrorFile
    $Output = Get-Content -LiteralPath $OutputFile -Raw
    $Errors = Get-Content -LiteralPath $ErrorFile -Raw
    if ($Process.ExitCode -ne 0) { throw "Installer exited $($Process.ExitCode): $Errors`n$Output" }
    if ($Output -notmatch 'Skipped live example \(noninteractive\)') { throw 'Installer did not confirm that the live example was skipped' }

    if (-not (Test-Path -LiteralPath $Aside -PathType Leaf) -or (Get-Item -LiteralPath $Aside).Length -le 0) { throw "Missing installed executable: $Aside" }
    if (Test-Path -LiteralPath $Inspect) { throw "Installer unexpectedly created Inspect: $Inspect" }
    $AsideVersion = & $Aside --version
    if ($LASTEXITCODE -ne 0 -or $AsideVersion -notmatch 'aside\s+\d+\.\d+\.\d+') { throw "Unexpected Aside version: $AsideVersion" }
    foreach ($Skill in $Skills) {
        if (-not (Test-Path -LiteralPath (Join-Path $Skill 'SKILL.md') -PathType Leaf) -or
            -not (Test-Path -LiteralPath (Join-Path $Skill '.aside-managed') -PathType Leaf)) {
            throw "Missing managed Aside skill: $Skill"
        }
    }
    Write-Host "Windows release install passed: $AsideVersion; no Inspect executable; $($Skills.Count) managed skills."
} finally {
    [Environment]::SetEnvironmentVariable('Path', $OriginalUserPath, 'User')
    Remove-Item -LiteralPath $Temp -Recurse -Force
}
