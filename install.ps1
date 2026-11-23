param([switch] $FromSource)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Net.Http
Add-Type -AssemblyName System.IO.Compression.FileSystem
$Repo = 'https://github.com/codesoda/aside'
$InstallDir = Join-Path $HOME '.aside/bin'
$Binary = Join-Path $InstallDir 'aside.exe'

function Get-Target {
    $Arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    switch ($Arch) {
        X64 { return 'x86_64-pc-windows-msvc' }
        Arm64 { return 'aarch64-pc-windows-msvc' }
        default { throw "Unsupported Windows architecture: $Arch" }
    }
}

function Get-LatestTag {
    # Disable redirects so a HEAD request reveals the public latest tag without the GitHub API.
    $Handler = [System.Net.Http.HttpClientHandler]::new()
    $Handler.AllowAutoRedirect = $false
    $Client = [System.Net.Http.HttpClient]::new($Handler)
    try {
        $Client.Timeout = [TimeSpan]::FromSeconds(30)
        $Response = $Client.SendAsync([System.Net.Http.HttpRequestMessage]::new('HEAD', "$Repo/releases/latest")).GetAwaiter().GetResult()
        try {
            if ([int]$Response.StatusCode -lt 300 -or [int]$Response.StatusCode -ge 400 -or -not $Response.Headers.Location) {
                throw "Latest release did not return a redirect (HTTP $([int]$Response.StatusCode))"
            }
            $Location = $Response.Headers.Location.ToString()
            if ($Location -notmatch '^https://github\.com/codesoda/aside/releases/tag/(v[0-9][a-zA-Z0-9._-]*)$' -and
                $Location -notmatch '^/codesoda/aside/releases/tag/(v[0-9][a-zA-Z0-9._-]*)$') {
                throw "Unexpected latest release redirect: $Location"
            }
            return $Matches[1]
        } finally { $Response.Dispose() }
    } finally { $Client.Dispose(); $Handler.Dispose() }
}

function Assert-SafeDestination([string] $Destination) {
    foreach ($Dir in @((Join-Path $HOME '.aside'), $InstallDir)) {
        if (Test-Path -LiteralPath $Dir) {
            $ExistingDir = Get-Item -LiteralPath $Dir -Force
            if ($ExistingDir.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing to install through reparse point $Dir" }
        }
    }
    if (Test-Path -LiteralPath $Destination) {
        $Existing = Get-Item -LiteralPath $Destination -Force
        if ($Existing.Attributes -band [IO.FileAttributes]::ReparsePoint -or $Existing.LinkType -or $Existing.PSIsContainer) {
            throw "Refusing to replace $Destination"
        }
    }
}

function Install-Binary([string] $Source) {
    Assert-SafeDestination $Binary
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Binary -Force

    $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $Entries = @($UserPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })
    if ($Entries.Count -eq 0) {
        $NewPath = if ([string]::IsNullOrEmpty($UserPath)) { $InstallDir } else { "$UserPath;$InstallDir" }
        [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
        Write-Host "Added $InstallDir to user PATH; open a new terminal to use aside."
    }
}

function Install-FromSource([string] $StagingDir) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'cargo is required to build from source' }
    if (-not (Get-Command pnpm -ErrorAction SilentlyContinue)) { throw 'pnpm is required to build the frontend from source' }
    & pnpm -C (Join-Path $PSScriptRoot 'app') install --frozen-lockfile
    if ($LASTEXITCODE -ne 0) { throw 'Frontend dependency install failed' }
    & pnpm -C (Join-Path $PSScriptRoot 'app') build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
    Push-Location $PSScriptRoot
    try {
        & cargo build --locked --release
        if ($LASTEXITCODE -ne 0) { throw 'Source build failed' }
    } finally { Pop-Location }
    $Source = Join-Path $PSScriptRoot 'target/release/aside.exe'
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw 'Source build did not produce aside.exe' }
    Copy-Item -LiteralPath $Source -Destination (Join-Path $StagingDir 'aside.exe')
}

function Install-FromRelease([string] $StagingDir) {
    $Target = Get-Target
    $Tag = Get-LatestTag
    $Asset = "aside-$Tag-$Target.zip"
    $Base = "$Repo/releases/download/$Tag"
    $TempDir = Join-Path ([IO.Path]::GetTempPath()) ("aside-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $TempDir | Out-Null
    try {
        $ArchivePath = Join-Path $TempDir $Asset
        $ChecksumsPath = Join-Path $TempDir 'checksums-sha256.txt'
        Invoke-WebRequest -Uri "$Base/checksums-sha256.txt" -OutFile $ChecksumsPath -UseBasicParsing
        Invoke-WebRequest -Uri "$Base/$Asset" -OutFile $ArchivePath -UseBasicParsing
        $Expected = @(
            foreach ($Line in (Get-Content -LiteralPath $ChecksumsPath)) {
                if ($Line -match '^([0-9a-fA-F]{64})\s+\*?(\S+)$' -and $Matches[2] -ceq $Asset) { $Matches[1] }
            }
        )
        if ($Expected.Count -ne 1) { throw "Expected exactly one SHA256 checksum for $Asset" }
        $Actual = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash
        if ($Actual -ine $Expected[0]) { throw "SHA256 mismatch for $Asset; refusing to install" }

        # Read only a single, exact-name regular entry; never expand arbitrary ZIP paths.
        $Zip = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
        try {
            if ($Zip.Entries.Count -ne 1 -or $Zip.Entries[0].FullName -cne 'aside.exe' -or
                $Zip.Entries[0].Length -le 0 -or $Zip.Entries[0].Length -gt 134217728 -or
                (($Zip.Entries[0].ExternalAttributes -shr 16) -band 61440) -eq 40960) {
                throw 'Archive must contain only a regular aside.exe (maximum 128 MiB)'
            }
            $Source = Join-Path $TempDir 'aside.exe'
            $InputStream = $Zip.Entries[0].Open()
            try {
                $OutputStream = [IO.File]::Create($Source)
                try { $InputStream.CopyTo($OutputStream) } finally { $OutputStream.Dispose() }
            } finally { $InputStream.Dispose() }
        } finally { $Zip.Dispose() }
        Copy-Item -LiteralPath $Source -Destination (Join-Path $StagingDir 'aside.exe')
    } finally { Remove-Item -LiteralPath $TempDir -Recurse -Force }
}

try {
    # Keep the existing Windows x64-only installer scope, including source mode.
    $Target = Get-Target
    if ($Target -ne 'x86_64-pc-windows-msvc') { throw 'This installer supports Windows x64 only; Windows ARM64 is not supported' }
    Assert-SafeDestination $Binary
    if ($FromSource -and (-not $PSScriptRoot -or
        -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml') -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'skills/aside/SKILL.md') -PathType Leaf) -or
        -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'app/package.json') -PathType Leaf))) {
        throw '-FromSource requires the Aside checkout with app/ and skills/aside/'
    }

    $StagingDir = Join-Path ([IO.Path]::GetTempPath()) ("aside-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $StagingDir | Out-Null
    try {
        if ($FromSource) {
            Install-FromSource $StagingDir
        } else {
            Install-FromRelease $StagingDir
        }
        $AsideSource = Join-Path $StagingDir 'aside.exe'
        if (-not (Test-Path -LiteralPath $AsideSource -PathType Leaf) -or
            (Get-Item -LiteralPath $AsideSource).Length -le 0) { throw 'Staged Aside binary is missing or empty' }

        # Keep the backup outside the install directory until verification and skill installation finish.
        $Backup = Join-Path $StagingDir 'aside.backup'
        $Existed = Test-Path -LiteralPath $Binary
        Assert-SafeDestination $Binary
        if ($Existed) { Copy-Item -LiteralPath $Binary -Destination $Backup }
        try {
            Install-Binary $AsideSource
            & $Binary --version
            if ($LASTEXITCODE -ne 0) { throw 'Installed binary failed to run' }
            & $Binary skill
            if ($LASTEXITCODE -ne 0) { throw 'Embedded skill installation failed' }
        } catch {
            $InstallError = $_
            try {
                Assert-SafeDestination $Binary
                if ($Existed) {
                    Copy-Item -LiteralPath $Backup -Destination $Binary -Force
                } elseif (Test-Path -LiteralPath $Binary) {
                    Remove-Item -LiteralPath $Binary -Force
                }
            } catch {
                Write-Warning "Could not restore ${Binary}: $_"
            }
            throw $InstallError
        }
    } finally {
        try { Remove-Item -LiteralPath $StagingDir -Recurse -Force }
        catch { Write-Warning "Could not clean up temporary installer files: $_" }
    }
    # User PATH changes do not update this PowerShell process or agents launched from it.
    $env:Path = "$InstallDir;$env:Path"
    Write-Host "Installed aside at $Binary."
    if (-not [Console]::IsInputRedirected) {
        $Answer = Read-Host 'Run a live Aside review example with an agent? [y/N]'
        if ($Answer -match '^(?i:y|yes)$') {
            $Agent = (Read-Host 'Choose agent (claude/codex/pi)').Trim().ToLowerInvariant()
            if ($Agent -notin @('claude', 'codex', 'pi')) { throw "Unknown agent: $Agent" }
            $Launcher = Get-Command $Agent -CommandType Application -ErrorAction SilentlyContinue
            if (-not $Launcher) { throw "Agent launcher not found: $Agent" }
            $Prompt = 'use aside to review https://github.com/cli/cli/pull/14921'
            & $Launcher.Source $Prompt
            if ($LASTEXITCODE -ne 0) { throw "$Agent live example failed" }
        } else { Write-Host 'Skipped live example.' }
    } else {
        Write-Host 'Skipped live example (noninteractive).'
    }
} catch {
    Write-Error "aside installation failed: $_"
    exit 1
}
