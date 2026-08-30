use std::{collections::BTreeSet, os::windows::process::CommandExt, path::Path, process::Command};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use sysinfo::System;

#[derive(Debug, Serialize)]
pub struct SystemSnapshot {
    captured_utc: String,
    computer_name: Option<String>,
    operating_system: Option<String>,
    operating_system_version: Option<String>,
    kernel_version: Option<String>,
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    total_swap_bytes: u64,
    cpus: Vec<CpuInfo>,
    windows_hardware_inventory: Option<serde_json::Value>,
    hardware_inventory_error: Option<String>,
    relevant_background_processes: Vec<String>,
    unavailable: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct CpuInfo {
    name: String,
    vendor_id: String,
    brand: String,
    frequency_mhz: u64,
}

pub fn capture(path: &Path) -> Result<()> {
    let mut system = System::new_all();
    system.refresh_all();

    let process_keywords = [
        "afterburner",
        "capframex",
        "discord",
        "gamebar",
        "hwinfo",
        "icue",
        "lghub",
        "obs",
        "overwolf",
        "radeonsoftware",
        "rtss",
        "steam",
    ];
    let mut background = BTreeSet::new();
    for process in system.processes().values() {
        let name = process.name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        if process_keywords
            .iter()
            .any(|keyword| lower.contains(keyword))
        {
            background.insert(name);
        }
    }

    let (windows_hardware_inventory, hardware_inventory_error) =
        match capture_windows_hardware_inventory() {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };

    let snapshot = SystemSnapshot {
        captured_utc: Utc::now().to_rfc3339(),
        computer_name: System::host_name(),
        operating_system: System::long_os_version().or_else(System::name),
        operating_system_version: System::os_version(),
        kernel_version: System::kernel_version(),
        total_memory_bytes: system.total_memory(),
        available_memory_bytes: system.available_memory(),
        total_swap_bytes: system.total_swap(),
        cpus: system
            .cpus()
            .iter()
            .map(|cpu| CpuInfo {
                name: cpu.name().to_string(),
                vendor_id: cpu.vendor_id().to_string(),
                brand: cpu.brand().to_string(),
                frequency_mhz: cpu.frequency(),
            })
            .collect(),
        windows_hardware_inventory,
        hardware_inventory_error,
        relevant_background_processes: background.into_iter().collect(),
        unavailable: vec![
            "CPU temperature (no safe universal Windows API)",
            "CPU package power (no safe universal Windows API)",
        ],
    };

    let json = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(path, json)
        .with_context(|| format!("failed to write system snapshot {}", path.display()))
}

fn capture_windows_hardware_inventory() -> Result<serde_json::Value> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r#"$ErrorActionPreference='SilentlyContinue';
$inventory=[ordered]@{
  bios=@(Get-CimInstance Win32_BIOS | Select-Object Manufacturer,SMBIOSBIOSVersion,ReleaseDate,SerialNumber);
  baseboard=@(Get-CimInstance Win32_BaseBoard | Select-Object Manufacturer,Product,Version,SerialNumber);
  computer_system=@(Get-CimInstance Win32_ComputerSystem | Select-Object Manufacturer,Model,SystemType,TotalPhysicalMemory,HypervisorPresent);
  processors=@(Get-CimInstance Win32_Processor | Select-Object Name,Manufacturer,ProcessorId,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,CurrentClockSpeed);
  physical_memory=@(Get-CimInstance Win32_PhysicalMemory | Select-Object BankLabel,DeviceLocator,Manufacturer,PartNumber,Capacity,Speed,ConfiguredClockSpeed,ConfiguredVoltage);
  video_controllers=@(Get-CimInstance Win32_VideoController | Select-Object Name,PNPDeviceID,DriverVersion,DriverDate,AdapterRAM,VideoProcessor,Status);
  display_drivers=@(Get-CimInstance Win32_PnPSignedDriver -Filter "DeviceClass='DISPLAY'" | Select-Object DeviceName,Manufacturer,DriverVersion,DriverDate,InfName,DeviceID);
  page_files=@(Get-CimInstance Win32_PageFileUsage | Select-Object Name,AllocatedBaseSize,CurrentUsage,PeakUsage);
  logical_disks=@(Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | Select-Object DeviceID,VolumeName,FileSystem,Size,FreeSpace);
  graphics_settings=@(Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\GraphicsDrivers' | Select-Object HwSchMode,TdrDelay,TdrDdiDelay)
};
$inventory | ConvertTo-Json -Depth 6 -Compress"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("failed to start PowerShell hardware inventory")?;
    if !output.status.success() {
        anyhow::bail!(
            "PowerShell hardware inventory returned {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).context("PowerShell hardware inventory returned invalid JSON")
}
