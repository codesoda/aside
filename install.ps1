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

function Install-Binary([string] $Source) {
    foreach ($Dir in @((Join-Path $HOME '.aside'), $InstallDir)) {
        if (Test-Path -LiteralPath $Dir) {
            $ExistingDir = Get-Item -LiteralPath $Dir -Force
            if ($ExistingDir.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing to install through reparse point $Dir" }
        }
    }
    if (Test-Path -LiteralPath $Binary) {
        $Existing = Get-Item -LiteralPath $Binary -Force
        if ($Existing.LinkType -or $Existing.PSIsContainer) { throw "Refusing to replace $Binary" }
    }
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

function Install-FromSource {
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
    Install-Binary $Source
}

function Install-FromRelease {
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
        Install-Binary $Source
    } finally { Remove-Item -LiteralPath $TempDir -Recurse -Force }
}

try {
    # Even when run from the repository checkout, install the latest release by default.
    if ($FromSource) {
        if (-not $PSScriptRoot -or
            -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'Cargo.toml') -PathType Leaf) -or
            -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'skills/aside/SKILL.md') -PathType Leaf) -or
            -not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'app/package.json') -PathType Leaf)) {
            throw '-FromSource requires the Aside checkout with app/ and skills/aside/'
        }
        Install-FromSource
    } else {
        Install-FromRelease
    }
    & $Binary --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed binary failed to run' }
    & $Binary skill
    if ($LASTEXITCODE -ne 0) { throw 'Embedded skill installation failed' }
    Write-Host "Installed aside at $Binary."
    if (-not [Console]::IsInputRedirected) {
        $Answer = Read-Host 'Run the interactive aside demo now? [y/N]'
        if ($Answer -match '^(?i:y|yes)$') {
            & $Binary demo
            if ($LASTEXITCODE -ne 0) { throw 'Demo failed' }
        } else { Write-Host 'Run aside demo when ready.' }
    } else {
        Write-Host 'Host agent: ask the user whether to run aside demo; run it only with explicit approval.'
    }
} catch {
    Write-Error "aside installation failed: $_"
    exit 1
}
