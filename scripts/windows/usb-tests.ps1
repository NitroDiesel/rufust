# End-to-end tests of the Windows disk engine on one disposable USB stick.
#
# EVERYTHING ON THE DRIVE IS DESTROYED. Run from an elevated PowerShell at the
# repository root, naming the drive by disk number and by serial:
#
#   Get-Disk | Select-Object Number, FriendlyName, BusType, SerialNumber
#   powershell -ExecutionPolicy Bypass -File scripts\windows\usb-tests.ps1 `
#       -DiskNumber 2 -Serial 000AEBFFD790BA60C30C0600
#
# The script and the tests both refuse a disk that is not a removable USB
# drive with exactly that serial. It runs the `windows::vhd_tests` engine
# tests (every file system provider, raw/gzip/VHDX image writes with
# read-back, Windows installer media) and the desktop's worker-pipe tests
# (format and cancellation), then leaves the stick as one FAT32 volume.
# Drives over 32 GiB also run the large FAT32 test.

param(
    [Parameter(Mandatory = $true)] [int] $DiskNumber,
    [Parameter(Mandatory = $true)] [string] $Serial,
    [string] $Log
)

$ErrorActionPreference = 'Stop'
if ($Log) { Start-Transcript -Path $Log -Force | Out-Null }

try {
    $principal = New-Object Security.Principal.WindowsPrincipal(
        [Security.Principal.WindowsIdentity]::GetCurrent())
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this script from an elevated (administrator) PowerShell.'
    }

    $disk = Get-Disk -Number $DiskNumber
    if ($disk.BusType -ne 'USB') { throw "Disk $DiskNumber is $($disk.BusType), not USB." }
    if ($disk.SerialNumber.Trim() -ne $Serial) {
        throw "Disk $DiskNumber has serial '$($disk.SerialNumber.Trim())', not '$Serial'."
    }
    if ($disk.IsBoot -or $disk.IsSystem) { throw "Disk $DiskNumber is a boot or system disk." }
    Write-Host "Erasing and testing disk ${DiskNumber}: $($disk.FriendlyName), $([math]::Round($disk.Size / 1GB, 1)) GB"

    $work = Join-Path ([IO.Path]::GetTempPath()) ("rufust-usb-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $work | Out-Null
    $source = Join-Path $work 'source.vhdx'
    $script = Join-Path $work 'diskpart.txt'
    Set-Content -Path $script -Encoding ascii -Value "create vdisk file=`"$source`" maximum=16 type=fixed"
    $output = & diskpart.exe /s $script
    if ($LASTEXITCODE -ne 0) { throw "diskpart failed:`n$($output -join "`n")" }

    $env:RUFUST_TEST_USB_SERIAL = $Serial
    $env:RUFUST_TEST_DISK = "$DiskNumber"
    $env:RUFUST_TEST_VHD_SOURCE = $source
    if ($disk.Size -gt 33GB) { $env:RUFUST_TEST_LARGE_DISK = "$DiskNumber" }

    cargo build -p rufust-helper --locked
    if ($LASTEXITCODE -ne 0) { throw 'build failed' }
    cargo test -p rufust-helper --locked windows::vhd_tests -- --ignored --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw 'engine tests failed on the USB drive' }
    cargo test -p rufust --locked -- --ignored --test-threads=1 helper_client::windows::tests
    if ($LASTEXITCODE -ne 0) { throw 'worker pipe tests failed on the USB drive' }

    # Leave the stick usable.
    Clear-Disk -Number $DiskNumber -RemoveData -RemoveOEM -Confirm:$false
    $style = (Get-Disk -Number $DiskNumber).PartitionStyle
    if ($style -eq 'RAW') { Initialize-Disk -Number $DiskNumber -PartitionStyle MBR }
    $size = [math]::Min((Get-Disk -Number $DiskNumber).LargestFreeExtent, 32GB - 1MB)
    New-Partition -DiskNumber $DiskNumber -Size $size -AssignDriveLetter |
        Format-Volume -FileSystem FAT32 -NewFileSystemLabel 'RUFUST' -Confirm:$false | Out-Null
    Write-Host 'USB TESTS PASSED'
}
catch {
    Write-Host "USB TESTS FAILED: $_"
    throw
}
finally {
    if ($work) { Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue }
    foreach ($name in 'RUFUST_TEST_USB_SERIAL', 'RUFUST_TEST_DISK', 'RUFUST_TEST_VHD_SOURCE', 'RUFUST_TEST_LARGE_DISK') {
        Remove-Item "Env:\$name" -ErrorAction SilentlyContinue
    }
    if ($Log) { Stop-Transcript | Out-Null }
}
