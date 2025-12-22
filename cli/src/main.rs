use std::{env, process};

use ledger_manager::{
    firmware::{
        get_latest_firmware_for_device, repair_device_in_bootloader_with_reconnect, update_firmware,
        FirmwareUpdatePhase,
    },
    genuine_check, install_bitcoin_app,
    ledger_transport_hidapi::{hidapi::HidApi, TransportNativeHID},
    list_installed_apps, open_bitcoin_app, update_bitcoin_app, DeviceInfo, InstallErr, UpdateErr,
};

// Print on stderr and exit with 1.
macro_rules! error {
    ($($arg:tt)*) => {{
        eprintln!($($arg)*);
        process::exit(1);
    }};
}

#[derive(Debug, Clone, Copy)]
enum Command {
    GetInfo,
    GenuineCheck,
    InstallMainApp,
    UpdateMainApp,
    OpenMainApp,
    InstallTestApp,
    UpdateTestApp,
    OpenTestApp,
    UpdateFirmware,
}

impl Command {
    /// Read command from environment variables.
    pub fn get() -> Option<Self> {
        let is_testnet = env::var("LEDGER_TESTNET").is_ok();
        let cmd_str = env::var("LEDGER_COMMAND").ok()?;

        if cmd_str == "getinfo" {
            Some(Self::GetInfo)
        } else if cmd_str == "genuinecheck" {
            Some(Self::GenuineCheck)
        } else if cmd_str == "installapp" {
            Some(if is_testnet {
                Self::InstallTestApp
            } else {
                Self::InstallMainApp
            })
        } else if cmd_str == "updateapp" {
            Some(if is_testnet {
                Self::UpdateTestApp
            } else {
                Self::UpdateMainApp
            })
        } else if cmd_str == "openapp" {
            Some(if is_testnet {
                Self::OpenTestApp
            } else {
                Self::OpenMainApp
            })
        } else if cmd_str == "updatefirm" {
            Some(Self::UpdateFirmware)
        } else {
            None
        }
    }
}

fn ledger_api() -> TransportNativeHID {
    let hid_api = match HidApi::new() {
        Ok(a) => a,
        Err(e) => error!("Error initializing HDI api: {}.", e),
    };
    match TransportNativeHID::new(&hid_api) {
        Ok(a) => a,
        Err(e) => error!("Error connecting to Ledger device: {}.", e),
    }
}

fn device_info(ledger_api: &TransportNativeHID) -> DeviceInfo {
    match DeviceInfo::new(ledger_api) {
        Ok(i) => i,
        Err(e) => error!("Error fetching device info: {}", e),
    }
}

fn print_ledger_info(ledger_api: &TransportNativeHID) {
    let device_info = device_info(ledger_api);
    println!("Information about the device: {:#?}", device_info);

    println!("Querying installed applications from your Ledger. You might have to confirm on your device.");
    let apps = match list_installed_apps(ledger_api) {
        Ok(a) => a,
        Err(e) => error!("Error listing installed applications: {}.", e),
    };
    println!("Installed applications:");
    for app in apps {
        println!("  - {:?}", app);
    }
}

fn perform_genuine_check(ledger_api: &TransportNativeHID) {
    println!("Querying Ledger's remote HSM to perform the genuine check. You might have to confirm the operation on your device.");
    if let Err(e) = genuine_check(ledger_api) {
        error!("Error when performing genuine check: {}", e);
    }
    println!("Success. Your Ledger is genuine.");
}

// Install the Bitcoin app on the device.
fn install_app(ledger_api: &TransportNativeHID, is_testnet: bool) {
    println!("You may have to allow on your device 1) listing installed apps 2) the Ledger manager to install the app.");
    match install_bitcoin_app(ledger_api, is_testnet) {
        Ok(()) => println!("Successfully installed the app."),
        Err(InstallErr::AlreadyInstalled) => {
            error!("Bitcoin app already installed. Use the update command to update it.")
        }
        Err(InstallErr::AppNotFound) => error!("Could not get info about Bitcoin app."),
        Err(InstallErr::Any(e)) => error!("Error installing Bitcoin app: {}.", e),
    }
}

fn update_app(ledger_api: &TransportNativeHID, is_testnet: bool) {
    println!("You may have to allow on your device 1) listing installed apps 2) the Ledger manager to install the app.");
    match update_bitcoin_app(ledger_api, is_testnet) {
        Ok(()) => println!("Successfully updated the app."),
        Err(UpdateErr::NotInstalled) => {
            error!("Bitcoin app isn't installed. Use the install command instead.")
        }
        Err(UpdateErr::AppNotFound) => error!("Could not get info about Bitcoin app."),
        Err(UpdateErr::AlreadyLatest) => error!("Bitcoin app is already at the latest version."),
        Err(UpdateErr::Any(e)) => error!("Error installing Bitcoin app: {}.", e),
    }
}

fn open_app(ledger_api: &TransportNativeHID, is_testnet: bool) {
    if let Err(e) = open_bitcoin_app(ledger_api, is_testnet) {
        error!("Error opening Bitcoin app: {}", e);
    }
}

/// Create a new HID API and transport connection to the Ledger device.
/// This is used for reconnection after device reboots.
fn create_transport() -> Result<TransportNativeHID, Box<dyn std::error::Error>> {
    // Create HidApi in a Box to ensure it lives long enough
    // The TransportNativeHID opens and owns the HidDevice, so HidApi
    // can be dropped after the device is opened.
    let hid_api = Box::new(HidApi::new()?);
    let transport = TransportNativeHID::new(&hid_api)?;
    // hid_api is dropped here, but the HidDevice inside transport is already open
    Ok(transport)
}

fn perform_firmware_update(ledger_api: TransportNativeHID) {
    // Get device info with the passed transport
    let device_info = match DeviceInfo::new(&ledger_api) {
        Ok(info) => info,
        Err(e) => error!("Error fetching device info: {}", e),
    };

    // Drop the transport to free the HID device handle BEFORE we try to create new connections
    // This is critical because HID devices typically only allow one open handle at a time
    drop(ledger_api);

    // Check if device is in bootloader mode (repair mode)
    if device_info.is_bootloader {
        println!("Device is in bootloader mode. Attempting repair...");
        println!("  Target ID: {:#x}", device_info.target_id);
        println!("  SE Target ID: {:#x}", device_info.se_target_id);
        println!("  Bootloader Version: {}", device_info.version);
        println!("  MCU Version: {:?}", device_info.mcu_version);
        println!("You may need to confirm operations on your device.");

        // Use the reconnect-capable repair function since device will reboot
        match repair_device_in_bootloader_with_reconnect(create_transport, |phase| {
            print_firmware_phase(&phase);
        }) {
            Ok(()) => println!("\nDevice repaired successfully!"),
            Err(e) => error!("\nRepair failed: {}", e),
        }
        return;
    }

    // Check for firmware updates
    println!("Checking for firmware updates...");
    let update_context = match get_latest_firmware_for_device(&device_info) {
        Ok(Some(ctx)) => ctx,
        Ok(None) => {
            println!(
                "Firmware is already up to date (version {}).",
                device_info.version
            );
            return;
        }
        Err(e) => error!("Error checking for updates: {}", e),
    };

    println!(
        "Update available: {} -> {}",
        device_info.version, update_context.final_firmware.version
    );
    println!("OSU: {}", update_context.osu.name);
    if update_context.should_flash_mcu {
        println!("Note: MCU will also be updated (device will reboot multiple times).");
    }
    println!();
    println!("You may need to confirm operations on your device.");
    println!("DO NOT disconnect your device during the update!");
    println!();

    // Create a fresh transport for the update
    let update_transport = match create_transport() {
        Ok(t) => t,
        Err(e) => error!("Error connecting to Ledger device: {}", e),
    };

    // Perform the update
    match update_firmware(&update_transport, &update_context, |phase| {
        print_firmware_phase(&phase);
    }) {
        Ok(()) => println!("\nFirmware updated successfully!"),
        Err(e) => error!("\nFirmware update failed: {}", e),
    }
}

fn print_firmware_phase(phase: &FirmwareUpdatePhase) {
    match phase {
        FirmwareUpdatePhase::CheckingForUpdates => {
            println!("Checking for updates...");
        }
        FirmwareUpdatePhase::DownloadingMetadata => {
            println!("Downloading firmware metadata...");
        }
        FirmwareUpdatePhase::InstallingOsu { progress } => {
            print!("\rInstalling OSU: {:>5.1}%", progress * 100.0);
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        FirmwareUpdatePhase::WaitingForBootloader => {
            println!("\nWaiting for device to reboot into bootloader...");
        }
        FirmwareUpdatePhase::FlashingMcu {
            iteration,
            progress,
        } => {
            print!(
                "\rFlashing MCU ({}/5): {:>5.1}%",
                iteration,
                progress * 100.0
            );
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        FirmwareUpdatePhase::InstallingFinalFirmware { progress } => {
            print!("\rInstalling final firmware: {:>5.1}%", progress * 100.0);
            std::io::Write::flush(&mut std::io::stdout()).ok();
        }
        FirmwareUpdatePhase::Completed => {
            println!("\nUpdate completed!");
        }
        FirmwareUpdatePhase::Failed { error } => {
            println!("\nUpdate failed: {}", error);
        }
    }
}

fn main() {
    let command = if let Some(cmd) = Command::get() {
        cmd
    } else {
        error!("Invalid or no command specified. The command must be passed through the LEDGER_COMMAND env var. Set LEDGER_TESTNET to use the Bitcoin testnet app instead where applicable.");
    };

    let ledger_api = ledger_api();
    match command {
        Command::GetInfo => {
            print_ledger_info(&ledger_api);
        }
        Command::GenuineCheck => {
            perform_genuine_check(&ledger_api);
        }
        Command::InstallMainApp => {
            install_app(&ledger_api, false);
        }
        Command::InstallTestApp => {
            install_app(&ledger_api, true);
        }
        Command::OpenMainApp => {
            open_app(&ledger_api, false);
        }
        Command::OpenTestApp => {
            open_app(&ledger_api, true);
        }
        Command::UpdateMainApp => {
            update_app(&ledger_api, false);
        }
        Command::UpdateTestApp => {
            update_app(&ledger_api, true);
        }
        Command::UpdateFirmware => {
            perform_firmware_update(ledger_api);
        }
    }
}
