//! Web WASM bindings for Ledger Manager.
//!
//! This crate provides JavaScript-callable functions for managing Ledger devices
//! from a web browser using the WebHID API.

use ledger_manager::transport::{is_webhid_supported, WebHidTransport};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// Initialize panic hook for better error messages in console.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Device information returned from get_device_info.
#[derive(Serialize, Deserialize)]
pub struct DeviceInfo {
    pub connected: bool,
    pub model: Option<String>,
    pub version: Option<String>,
    pub bitcoin_installed: bool,
    pub bitcoin_version: Option<String>,
    pub bitcoin_test_installed: bool,
    pub bitcoin_test_version: Option<String>,
}

/// Result of an operation.
#[derive(Serialize, Deserialize)]
pub struct OperationResult {
    pub success: bool,
    pub message: String,
}

/// Check if WebHID is supported in the current browser.
#[wasm_bindgen]
pub fn check_webhid_support() -> bool {
    is_webhid_supported()
}

/// Request access to a Ledger device.
///
/// This must be called from a user gesture (button click).
/// Returns a promise that resolves to true if successful.
#[wasm_bindgen]
pub async fn connect_device() -> Result<JsValue, JsValue> {
    match WebHidTransport::request_device().await {
        Ok(_transport) => {
            let result = OperationResult {
                success: true,
                message: "Device connected successfully".to_string(),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
        Err(e) => {
            let result = OperationResult {
                success: false,
                message: format!("Failed to connect: {}", e),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
    }
}

/// Get device information.
///
/// This attempts to connect to a previously authorized device and query its info.
#[wasm_bindgen]
pub async fn get_device_info() -> Result<JsValue, JsValue> {
    let info = DeviceInfo {
        connected: false,
        model: None,
        version: None,
        bitcoin_installed: false,
        bitcoin_version: None,
        bitcoin_test_installed: false,
        bitcoin_test_version: None,
    };

    // For now, return a placeholder - full implementation requires
    // integrating with the ledger_manager functions
    Ok(serde_wasm_bindgen::to_value(&info)?)
}

/// Install the Bitcoin app on the device.
#[wasm_bindgen]
pub async fn install_bitcoin_app(testnet: bool) -> Result<JsValue, JsValue> {
    let app_name = if testnet { "Bitcoin Test" } else { "Bitcoin" };

    // Placeholder - full implementation requires HTTP and WebSocket clients
    let result = OperationResult {
        success: false,
        message: format!(
            "Installing {} app... (Full implementation in progress)",
            app_name
        ),
    };

    Ok(serde_wasm_bindgen::to_value(&result)?)
}

/// Open the Bitcoin app on the device.
#[wasm_bindgen]
pub async fn open_bitcoin_app(testnet: bool) -> Result<JsValue, JsValue> {
    let app_name = if testnet { "Bitcoin Test" } else { "Bitcoin" };

    // Placeholder - full implementation requires transport integration
    let result = OperationResult {
        success: false,
        message: format!(
            "Opening {} app... (Full implementation in progress)",
            app_name
        ),
    };

    Ok(serde_wasm_bindgen::to_value(&result)?)
}

/// Perform genuine check on the device.
#[wasm_bindgen]
pub async fn genuine_check() -> Result<JsValue, JsValue> {
    // Placeholder - requires WebSocket client for HSM communication
    let result = OperationResult {
        success: false,
        message: "Genuine check... (Full implementation in progress)".to_string(),
    };

    Ok(serde_wasm_bindgen::to_value(&result)?)
}

/// Log a message to the browser console.
#[wasm_bindgen]
pub fn log(message: &str) {
    web_sys::console::log_1(&JsValue::from_str(message));
}
