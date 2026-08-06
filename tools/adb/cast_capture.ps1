# Capture Cast V2 traffic from an Android device with tcpdump over ADB.
#
# Usage:
#   .\tools\adb\cast_capture.ps1 [[-Device] <serial>] [[-OutDir] <path>]
#
# Example:
#   .\tools\adb\cast_capture.ps1 -Device 192.168.1.10:5555 -OutDir .\capture
#
# Requires: adb, a (preferably rooted) device, tcpdump on the device.
param(
    [string]$Device = "",
    [string]$OutDir = ".\capture"
)

if (-not $Device) {
    $lines = adb devices
    if ($lines.Count -ge 2) {
        $Device = ($lines[1] -split "`t")[0]
    }
}
if (-not $Device) {
    Write-Error "No ADB device found. Pass the serial explicitly."
    exit 1
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$Pcap = "/sdcard/cast_$(Get-Date -UFormat %s).pcap"

Write-Host "Device: $Device"
Write-Host "Capturing to $Pcap on the device. Cast something now."
Write-Host "Press Ctrl-C here when done; the pcap will be pulled automatically."

# Start tcpdump in the background via adb shell (it stays on the device).
adb -s $Device shell "su -c 'tcpdump -i any -s 0 -w $Pcap'"

Write-Host "Pulling pcap..."
adb -s $Device pull $Pcap "$OutDir\cast.pcap"
adb -s $Device shell "su -c 'rm -f $Pcap'" | Out-Null
Write-Host "Saved to $OutDir\cast.pcap - open it in Wireshark and filter:"
Write-Host "  mdns  |  tcp.port == 8009  |  tls.handshake.type == 11"
