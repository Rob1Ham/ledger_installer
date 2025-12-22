//! Operations module - extracted functions for Ledger device operations.
//!
//! All functions return `Result<String, String>` for consistent error handling
//! that works with both interactive menu and legacy CLI modes.

use ledger_manager::{
    genuine_check, install_bitcoin_app,
    ledger_transport_hidapi::{hidapi::HidApi, TransportNativeHID},
    list_installed_apps, open_bitcoin_app, update_bitcoin_app, DeviceInfo, InstallErr, UpdateErr,
};

/// Initialize the HID API and connect to a Ledger device.
pub fn connect_ledger() -> Result<TransportNativeHID, String> {
    let hid_api = HidApi::new().map_err(|e| format!("Error initializing HID API: {}.", e))?;
    TransportNativeHID::new(&hid_api)
        .map_err(|e| format!("Error connecting to Ledger device: {}.", e))
}

/// Get device information from a connected Ledger.
pub fn get_device_info(ledger_api: &TransportNativeHID) -> Result<DeviceInfo, String> {
    DeviceInfo::new(ledger_api).map_err(|e| format!("Error fetching device info: {}", e))
}

/// Get and display information about the device and installed applications.
pub fn print_ledger_info(ledger_api: &TransportNativeHID) -> Result<String, String> {
    let device_info = get_device_info(ledger_api)?;
    let mut output = format!("Information about the device: {:#?}\n", device_info);

    output.push_str(
        "Querying installed applications from your Ledger. You might have to confirm on your device.\n",
    );

    let apps = list_installed_apps(ledger_api)
        .map_err(|e| format!("Error listing installed applications: {}.", e))?;

    output.push_str("Installed applications:\n");
    for app in apps {
        output.push_str(&format!("  - {:?}\n", app));
    }

    Ok(output)
}

/// Perform a genuine check on the Ledger device.
pub fn perform_genuine_check(ledger_api: &TransportNativeHID) -> Result<String, String> {
    genuine_check(ledger_api).map_err(|e| format!("Error when performing genuine check: {}", e))?;
    Ok("Success. Your Ledger is genuine.".to_string())
}

/// Install the Bitcoin app on the device.
pub fn install_app(ledger_api: &TransportNativeHID, is_testnet: bool) -> Result<String, String> {
    let app_name = if is_testnet {
        "Bitcoin Test"
    } else {
        "Bitcoin"
    };

    match install_bitcoin_app(ledger_api, is_testnet) {
        Ok(()) => Ok(format!("Successfully installed the {} app.", app_name)),
        Err(InstallErr::AlreadyInstalled) => Err(format!(
            "{} app already installed. Use the update command to update it.",
            app_name
        )),
        Err(InstallErr::AppNotFound) => Err(format!("Could not get info about {} app.", app_name)),
        Err(InstallErr::Any(e)) => Err(format!("Error installing {} app: {}.", app_name, e)),
    }
}

/// Update the Bitcoin app on the device.
pub fn update_app(ledger_api: &TransportNativeHID, is_testnet: bool) -> Result<String, String> {
    let app_name = if is_testnet {
        "Bitcoin Test"
    } else {
        "Bitcoin"
    };

    match update_bitcoin_app(ledger_api, is_testnet) {
        Ok(()) => Ok(format!("Successfully updated the {} app.", app_name)),
        Err(UpdateErr::NotInstalled) => Err(format!(
            "{} app isn't installed. Use the install command instead.",
            app_name
        )),
        Err(UpdateErr::AppNotFound) => Err(format!("Could not get info about {} app.", app_name)),
        Err(UpdateErr::AlreadyLatest) => Err(format!(
            "{} app is already at the latest version.",
            app_name
        )),
        Err(UpdateErr::Any(e)) => Err(format!("Error updating {} app: {}.", app_name, e)),
    }
}

/// Open the Bitcoin app on the device.
pub fn open_app(ledger_api: &TransportNativeHID, is_testnet: bool) -> Result<String, String> {
    let app_name = if is_testnet {
        "Bitcoin Test"
    } else {
        "Bitcoin"
    };

    open_bitcoin_app(ledger_api, is_testnet)
        .map_err(|e| format!("Error opening {} app: {}", app_name, e))?;
    Ok(format!("{} app opened successfully.", app_name))
}
