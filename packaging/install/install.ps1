# Installs Meridian on Windows from a published release.
#
#   irm https://github.com/ZanattaMichael/meridian-core/releases/latest/download/install.ps1 | iex
#
# It downloads the archive for this machine, checks it against the release's
# checksums, and unpacks meridian.exe and its plugins together into one
# directory. Nothing is installed outside that directory, and nothing needs
# an administrator.
#
# Settings, all optional, read from the environment:
#
#   MERIDIAN_VERSION        the version to install, as in 0.1.0. Default: the latest release.
#   MERIDIAN_INSTALL_DIR    where to put it. Default: %LOCALAPPDATA%\Meridian\bin
#   MERIDIAN_VERIFY         "cosign" to refuse to install unless cosign has
#                           verified the checksums' signature; "none" to skip
#                           that check. Default: verify when cosign is installed.
#   MERIDIAN_DOWNLOAD_BASE  where the release's files are, in place of GitHub:
#                           a URL or a directory. For a mirror, or for testing.
#   MERIDIAN_NO_PATH        set to leave the user's PATH alone.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repository = 'ZanattaMichael/meridian-core'
# Who signs a release: this repository's release workflow, run for a version tag.
$signerIdentity = "^https://github\.com/$repository/\.github/workflows/release\.yml@refs/tags/v[0-9]"
$signerIssuer = 'https://token.actions.githubusercontent.com'

function Fail([string] $message) {
    throw "install.ps1: $message"
}

# Copies one of the release's files to a local path. Returns $false when the
# release does not have the file.
function Get-ReleaseFile([string] $base, [string] $name, [string] $to) {
    if ($base -match '^https?://') {
        try {
            Invoke-WebRequest -Uri "$base/$name" -OutFile $to -UseBasicParsing
            return $true
        }
        catch {
            return $false
        }
    }
    $from = Join-Path $base $name
    if (Test-Path -LiteralPath $from -PathType Leaf) {
        Copy-Item -LiteralPath $from -Destination $to
        return $true
    }
    return $false
}

$architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$arch = switch ($architecture) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { Fail "there is no release for a $architecture processor" }
}
$target = "$arch-pc-windows-msvc"

$installDir = if ($env:MERIDIAN_INSTALL_DIR) { $env:MERIDIAN_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Meridian\bin' }
$verify = if ($env:MERIDIAN_VERIFY) { $env:MERIDIAN_VERIFY } else { 'auto' }
$version = "$env:MERIDIAN_VERSION".TrimStart('v')

$work = Join-Path ([System.IO.Path]::GetTempPath()) "meridian-install-$([System.Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $versionFile = Join-Path $work 'VERSION'
    if ($env:MERIDIAN_DOWNLOAD_BASE) {
        $base = $env:MERIDIAN_DOWNLOAD_BASE.TrimEnd('/', '\')
        if (-not $version) {
            if (-not (Get-ReleaseFile $base 'VERSION' $versionFile)) { Fail "no VERSION file at $base" }
            $version = (Get-Content -LiteralPath $versionFile -Raw).Trim()
        }
    }
    elseif ($version) {
        $base = "https://github.com/$repository/releases/download/v$version"
    }
    else {
        $latest = "https://github.com/$repository/releases/latest/download"
        if (-not (Get-ReleaseFile $latest 'VERSION' $versionFile)) { Fail 'could not find the latest release of Meridian' }
        $version = (Get-Content -LiteralPath $versionFile -Raw).Trim()
        $base = "https://github.com/$repository/releases/download/v$version"
    }
    if ($version -notmatch '^[0-9A-Za-z.+-]+$') { Fail "`"$version`" is not a version" }

    $archive = "meridian-$version-$target.zip"
    $checksums = "meridian-$version-checksums.txt"
    Write-Host "Installing Meridian $version for $target"

    $checksumsPath = Join-Path $work $checksums
    $archivePath = Join-Path $work $archive
    if (-not (Get-ReleaseFile $base $checksums $checksumsPath)) { Fail "could not download $checksums from $base" }
    if (-not (Get-ReleaseFile $base $archive $archivePath)) { Fail "could not download $archive from $base" }

    # The checksums are what the archive is checked against, so they are
    # checked first: against the signature the release workflow made for them.
    if ($verify -notin @('auto', 'cosign', 'none')) {
        Fail "MERIDIAN_VERIFY is `"$verify`"; use cosign or none, or leave it unset"
    }
    if ($verify -ne 'none') {
        $bundle = Join-Path $work "$checksums.sigstore.json"
        if (-not (Get-Command cosign -ErrorAction SilentlyContinue)) {
            if ($verify -eq 'cosign') { Fail 'MERIDIAN_VERIFY=cosign, and cosign is not installed' }
            Write-Host "cosign is not installed, so the release's signature was not checked; the download is still checked against its checksums"
        }
        elseif (-not (Get-ReleaseFile $base "$checksums.sigstore.json" $bundle)) {
            if ($verify -eq 'cosign') { Fail 'this release has no signature for its checksums' }
            Write-Host 'this release has no signature for its checksums, so none was checked'
        }
        else {
            $ErrorActionPreference = 'Continue'
            $said = & cosign verify-blob --bundle $bundle `
                --certificate-identity-regexp $signerIdentity `
                --certificate-oidc-issuer $signerIssuer `
                $checksumsPath 2>&1
            $ErrorActionPreference = 'Stop'
            if ($LASTEXITCODE -ne 0) { Fail "the checksums' signature does not verify: $said" }
            Write-Host "The release's signature verifies"
        }
    }

    $expected = $null
    foreach ($line in Get-Content -LiteralPath $checksumsPath) {
        $fields = $line -split '\s+', 2
        if ($fields.Count -eq 2 -and $fields[1].Trim() -eq $archive) { $expected = $fields[0] }
    }
    if (-not $expected) { Fail "$checksums does not list $archive" }
    $actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
    if ($expected -ne $actual) {
        Fail "$archive does not match its checksum (expected $expected, got $($actual.ToLower())); nothing was installed"
    }

    $unpackedRoot = Join-Path $work 'unpacked'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $unpackedRoot
    $unpacked = Join-Path $unpackedRoot "meridian-$version-$target"
    if (-not (Test-Path -LiteralPath (Join-Path $unpacked 'meridian.exe'))) {
        Fail "$archive does not hold meridian.exe"
    }

    # Plugins from an earlier version are removed, so a plugin that a release
    # dropped does not linger beside a meridian that never shipped with it.
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Get-ChildItem -LiteralPath $installDir -Filter 'meridian-target-*' | Remove-Item -Force
    Copy-Item -Path (Join-Path $unpacked '*') -Destination $installDir -Force

    $meridian = Join-Path $installDir 'meridian.exe'
    $installed = & $meridian --version
    if ($LASTEXITCODE -ne 0) { Fail 'the installed meridian does not run on this machine' }
    Write-Host "Installed $installed in $installDir"

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onPath = ($userPath -split ';') -contains $installDir
    if (-not $onPath -and -not $env:MERIDIAN_NO_PATH) {
        $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Host "Added $installDir to your PATH. Open a new terminal to use the meridian command."
    }
    elseif (-not $onPath) {
        Write-Host "$installDir is not on your PATH."
    }
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
