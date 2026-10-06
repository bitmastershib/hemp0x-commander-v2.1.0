<#
.SYNOPSIS
    Creates a blockchain snapshot archive (.7z or .zip) and manifest.json entry
    for Hemp0x or Ravencoin node bootstrap fast-syncing.

.EXAMPLE
    .\create-snapshot.ps1 -Chain hemp0x -DataDir "$env:APPDATA\Hemp0x" -OutDir "C:\snapshots" -BaseUrl "https://hemp0x.com/snapshots"
    .\create-snapshot.ps1 -Chain ravencoin -DataDir "$env:APPDATA\Raven" -OutDir "C:\snapshots" -BaseUrl "https://hemp0x.com/snapshots"
#>

param (
    [Parameter(Mandatory=$true)]
    [ValidateSet("hemp0x", "ravencoin")]
    [string]$Chain,

    [Parameter(Mandatory=$true)]
    [string]$DataDir,

    [Parameter(Mandatory=$true)]
    [string]$OutDir,

    [Parameter(Mandatory=$true)]
    [string]$BaseUrl,

    [ValidateSet("7z", "zip")]
    [string]$Format = "7z",

    [int]$BlockHeight = 0,
    [string]$Notes = ""
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $DataDir)) {
    Write-Error "Data directory not found: $DataDir"
}

if (-not (Test-Path $OutDir)) {
    New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
}

$date = Get-Date -Format "yyyy-MM-dd"
$archiveName = "$Chain-snapshot-$date.$Format"
$archivePath = Join-Path $OutDir $archiveName
$manifestPath = Join-Path $OutDir "manifest.json"

Write-Host "=== Creating $Chain Snapshot Archive ($Format) ===" -ForegroundColor Cyan
Write-Host "Source: $DataDir"
Write-Host "Target: $archivePath"

$includeFolders = @("blocks", "chainstate", "assets", "messages", "restricted", "rewards", "indexes")
$validFolders = @()

foreach ($folder in $includeFolders) {
    $p = Join-Path $DataDir $folder
    if (Test-Path $p) {
        $validFolders += $p
        Write-Host "  + Including $folder" -ForegroundColor Green
    }
}

if ($validFolders.Count -eq 0) {
    Write-Error "No valid chain folders (blocks, chainstate) found in $DataDir"
}

if (Test-Path $archivePath) {
    Remove-Item -Path $archivePath -Force
}

# Measure extracted size
$extractedBytes = 0
foreach ($folder in $validFolders) {
    $size = (Get-ChildItem $folder -Recurse -File -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    $extractedBytes += $size
}

# Compression
if ($Format -eq "7z") {
    $sevenZip = "C:\Program Files\7-Zip\7z.exe"
    if (-not (Test-Path $sevenZip)) {
        $sevenZip = (Get-Command "7z" -ErrorAction SilentlyContinue).Path
    }
    if (-not $sevenZip) {
        Write-Error "7-Zip executable (7z.exe) not found. Please install 7-Zip."
    }
    
    $argsList = @("a", "-t7z", "-mx=5", "-mmt=on", $archivePath) + $validFolders
    & $sevenZip $argsList
} else {
    Compress-Archive -Path $validFolders -DestinationPath $archivePath -CompressionLevel Optimal
}

if (-not (Test-Path $archivePath)) {
    Write-Error "Failed to create archive: $archivePath"
}

$fileSize = (Get-Item $archivePath).Length
Write-Host "Archive created. Size: $([math]::Round($fileSize/1MB, 2)) MB ($fileSize bytes)" -ForegroundColor Green

Write-Host "Computing SHA-256 checksum..." -ForegroundColor Cyan
$sha256 = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLower()
Write-Host "SHA-256: $sha256" -ForegroundColor Yellow

# Read or initialize manifest.json
$manifest = @{}
if (Test-Path $manifestPath) {
    try {
        $json = Get-Content $manifestPath -Raw | ConvertFrom-Json
        foreach ($prop in $json.psobject.Properties) {
            $manifest[$prop.Name] = $prop.Value
        }
    } catch {
        Write-Warning "Existing manifest.json corrupt, overwriting."
    }
}

$downloadUrl = "$BaseUrl/$archiveName"

$entry = [ordered]@{
    url            = $downloadUrl
    sha256         = $sha256
    size           = $fileSize
    extracted_size = $extractedBytes
    height         = $BlockHeight
    date           = $date
    format         = $Format
    notes          = $Notes
}

$manifest[$Chain] = $entry

$jsonOutput = $manifest | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText($manifestPath, $jsonOutput, [System.Text.Encoding]::UTF8)

Write-Host "`nUpdated $manifestPath cleanly!" -ForegroundColor Green
Write-Host "Upload contents of $OutDir to $BaseUrl to publish the new snapshot." -ForegroundColor Cyan
