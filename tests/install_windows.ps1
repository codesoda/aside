# Native release smoke test. GitHub-hosted Windows runners have disposable user profiles.
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Installer = Join-Path $Root 'install.ps1'
$Bin = Join-Path $HOME '.aside/bin'
$Aside = Join-Path $Bin 'aside.exe'
$Inspect = Join-Path $Bin 'inspect.exe'
$InspectSHA256 = '29b26dd277c373f74f5aea9bef5661a42fe9f4c0fce72dd75c353ad796f2f1b1'
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
    function Invoke-Installer([string[]] $Options = @(), [switch] $ExpectFailure) {
        # Redirect stdin so the installer cannot prompt for or launch a live agent demo.
        $Arguments = @('-NoProfile', '-NonInteractive', '-File', "`"$Installer`"") + $Options
        $Process = Start-Process -FilePath 'pwsh.exe' -ArgumentList $Arguments -Wait -PassThru -RedirectStandardInput $InputFile -RedirectStandardOutput $OutputFile -RedirectStandardError $ErrorFile
        $Output = Get-Content -LiteralPath $OutputFile -Raw
        $Errors = Get-Content -LiteralPath $ErrorFile -Raw
        if ($ExpectFailure) {
            if ($Process.ExitCode -eq 0) { throw "Installer unexpectedly succeeded: $Output" }
            return "$Errors`n$Output"
        }
        if ($Process.ExitCode -ne 0) { throw "Installer exited $($Process.ExitCode): $Errors`n$Output" }
        if ($Output -notmatch 'Skipped live example \(noninteractive\)') { throw 'Installer did not confirm that the live example was skipped' }
        return $Output
    }
    $Output = Invoke-Installer -Options @('-WithoutInspect')

    if (-not (Test-Path -LiteralPath $Aside -PathType Leaf) -or (Get-Item -LiteralPath $Aside).Length -le 0) { throw "Missing installed executable: $Aside" }
    if ((Test-Path -LiteralPath $Inspect) -or $Output -match 'Inspect is licensed|Installed Inspect|Inspect installation failed') {
        throw 'Explicit skip attempted to install Inspect'
    }
    $AsideVersion = & $Aside --version
    if ($LASTEXITCODE -ne 0 -or $AsideVersion -notmatch 'aside\s+\d+\.\d+\.\d+') { throw "Unexpected Aside version: $AsideVersion" }
    foreach ($Skill in $Skills) {
        if (-not (Test-Path -LiteralPath (Join-Path $Skill 'SKILL.md') -PathType Leaf) -or
            -not (Test-Path -LiteralPath (Join-Path $Skill '.aside-managed') -PathType Leaf)) {
            throw "Missing managed Aside skill: $Skill"
        }
    }
    $DefaultOutput = Invoke-Installer
    if ($DefaultOutput -notmatch 'Inspect is licensed under FSL-1\.1-ALv2:' -or
        $DefaultOutput -notmatch 'Installed Inspect v0\.1\.1') { throw 'Default install did not report the Inspect licence and installation' }
    if (-not (Test-Path -LiteralPath $Inspect -PathType Leaf) -or
        (Get-FileHash -LiteralPath $Inspect -Algorithm SHA256).Hash -ine $InspectSHA256) {
        throw 'Default install did not produce the official verified Inspect executable'
    }
    $Output = Invoke-Installer -Options @('-WithoutInspect')
    if ($Output -match 'Inspect is licensed|Installed Inspect|Inspect installation failed' -or
        (Get-FileHash -LiteralPath $Inspect -Algorithm SHA256).Hash -ine $InspectSHA256) {
        throw 'Explicit skip attempted Inspect or changed its installed executable'
    }
    $Output = Invoke-Installer -Options @('-WithInspect')
    if ($Output -notmatch 'Inspect v0\.1\.1 is already installed' -or
        (Get-FileHash -LiteralPath $Inspect -Algorithm SHA256).Hash -ine $InspectSHA256) {
        throw 'Explicit -WithInspect did not retain the verified executable'
    }
    $DifferentInspect = 'pre-existing executable bytes'
    [IO.File]::WriteAllText($Inspect, $DifferentInspect)
    $Output = Invoke-Installer
    if ($Output -notmatch 'Refusing to replace a different Inspect executable' -or
        [IO.File]::ReadAllText($Inspect) -cne $DifferentInspect) {
        throw 'Default reinstall did not preserve a different existing executable'
    }
    $Output = Invoke-Installer -Options @('-WithInspect', '-WithoutInspect') -ExpectFailure
    if ($Output -notmatch 'Cannot specify both -WithInspect and -WithoutInspect' -or
        [IO.File]::ReadAllText($Inspect) -cne $DifferentInspect) {
        throw 'Conflicting Inspect switches were not rejected before installation'
    }
    Write-Host "Windows release install passed: $AsideVersion; fresh skip; default verified Inspect SHA256; skip retention; explicit alias; collision refusal; conflicting switches; $($Skills.Count) managed skills."
} finally {
    [Environment]::SetEnvironmentVariable('Path', $OriginalUserPath, 'User')
    Remove-Item -LiteralPath $Temp -Recurse -Force
}
