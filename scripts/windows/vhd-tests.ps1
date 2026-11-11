# End-to-end tests of the Windows disk engine on disposable virtual disks.
#
# Run from an elevated PowerShell at the repository root:
#   powershell -ExecutionPolicy Bypass -File scripts\windows\vhd-tests.ps1
#
# Creates a 2 GiB and a sparse 40 GiB VHDX in a temporary folder and attaches
# them, plus a detached 16 MiB VHDX used as a source image. It runs the
# ignored `windows::vhd_tests` (helper engine) and the desktop's worker-pipe
# and VHD preview tests, then detaches and deletes everything. The tests
# refuse any disk that is not file-backed virtual storage.

$ErrorActionPreference = 'Stop'

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script from an elevated (administrator) PowerShell.'
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("rufust-vhd-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$small = Join-Path $work 'small.vhdx'
$large = Join-Path $work 'large.vhdx'
$source = Join-Path $work 'source.vhdx'

function Invoke-DiskPart([string[]] $commands) {
    $script = Join-Path $work 'diskpart.txt'
    Set-Content -Path $script -Value $commands -Encoding ascii
    $output = & diskpart.exe /s $script
    if ($LASTEXITCODE -ne 0) {
        throw "diskpart failed:`n$($output -join "`n")"
    }
}

function Mount-TestDisk([string] $path, [int] $megabytes) {
    Invoke-DiskPart @(
        "create vdisk file=`"$path`" maximum=$megabytes type=expandable",
        "select vdisk file=`"$path`"",
        'attach vdisk'
    )
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        $number = (Get-DiskImage -ImagePath $path).Number
        if ($null -ne $number) {
            return $number
        }
        Start-Sleep -Milliseconds 250
    }
    throw "Windows did not report a disk number for $path"
}

try {
    $env:RUFUST_TEST_DISK = Mount-TestDisk $small 2048
    $env:RUFUST_TEST_LARGE_DISK = Mount-TestDisk $large 40960
    Invoke-DiskPart @("create vdisk file=`"$source`" maximum=16 type=fixed")
    $env:RUFUST_TEST_VHD_SOURCE = $source
    Write-Host "Testing on disk $env:RUFUST_TEST_DISK and large disk $env:RUFUST_TEST_LARGE_DISK"
    cargo build -p rufust-helper --locked
    cargo test -p rufust-helper --locked windows::vhd_tests -- --ignored --test-threads=1
    if ($LASTEXITCODE -ne 0) {
        throw 'virtual disk tests failed'
    }
    cargo test -p rufust --locked -- --ignored --test-threads=1 helper_client::windows::tests image_inspection::windows_tests
    if ($LASTEXITCODE -ne 0) {
        throw 'worker pipe tests failed'
    }
}
finally {
    foreach ($path in @($small, $large)) {
        if (Test-Path $path) {
            try {
                Invoke-DiskPart @("select vdisk file=`"$path`"", 'detach vdisk')
            }
            catch {
                Write-Warning "Could not detach ${path}: $_"
            }
        }
    }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    Remove-Item Env:\RUFUST_TEST_DISK -ErrorAction SilentlyContinue
    Remove-Item Env:\RUFUST_TEST_LARGE_DISK -ErrorAction SilentlyContinue
}
