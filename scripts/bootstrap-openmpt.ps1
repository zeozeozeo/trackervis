param(
    [string]$Version = "0.8.0",
    [string]$Destination = ".vendor/libopenmpt"
)

$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$destinationPath = Join-Path $root $Destination
$archivePath = Join-Path $root ".vendor/libopenmpt-$Version.zip"
$url = "https://lib.openmpt.org/files/libopenmpt/dev/libopenmpt-$Version+release.dev.windows.vs2022.zip"

New-Item -ItemType Directory -Force -Path (Split-Path $destinationPath -Parent) | Out-Null

Write-Host "Downloading libopenmpt $Version from $url"
Invoke-WebRequest -Uri $url -OutFile $archivePath

if (Test-Path $destinationPath) {
    Remove-Item -Recurse -Force $destinationPath
}

Expand-Archive -Path $archivePath -DestinationPath $destinationPath -Force
Write-Host "libopenmpt unpacked to $destinationPath"
