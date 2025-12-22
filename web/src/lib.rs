//! Web WASM bindings for Ledger Manager.
//!
//! This crate provides JavaScript-callable functions for managing Ledger devices
//! from a web browser using the WebHID API.

use ledger_manager::ledger_apdu::APDUCommand;
use ledger_manager::transport::{is_webhid_supported, WebHidTransport};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

// Global transport instance (stored between calls)
thread_local! {
    static TRANSPORT: RefCell<Option<Rc<WebHidTransport>>> = const { RefCell::new(None) };
}

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
    pub mcu_version: Option<String>,
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
        Ok(transport) => {
            // Store the transport for future use
            let transport = Rc::new(transport);
            TRANSPORT.with(|t| {
                *t.borrow_mut() = Some(transport);
            });

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

// APDU commands
const GET_VERSION: APDUCommand<&[u8]> = APDUCommand {
    cla: 0xe0,
    ins: 0x01,
    p1: 0x00,
    p2: 0x00,
    data: &[],
};

const LIST_APPS: APDUCommand<&[u8]> = APDUCommand {
    cla: 0xe0,
    ins: 0xde,
    p1: 0x00,
    p2: 0x00,
    data: &[],
};

const CONTINUE_LIST_APPS: APDUCommand<&[u8]> = APDUCommand {
    cla: 0xe0,
    ins: 0xdf,
    p1: 0x00,
    p2: 0x00,
    data: &[],
};

/// Parse device version info from APDU response
fn parse_version_info(data: &[u8]) -> Option<(String, Option<String>)> {
    if data.len() < 5 {
        return None;
    }

    let mut i = 4; // Skip target_id
    let ver_len = data[i] as usize;
    i += 1;

    if data.len() < i + ver_len + 1 {
        return None;
    }

    let version = std::str::from_utf8(&data[i..i + ver_len]).ok()?.to_string();
    i += ver_len;

    let flags_len = data[i] as usize;
    i += 1 + flags_len;

    // Try to get MCU version if available
    let mcu_version = if data.len() > i {
        let mcu_len = data[i] as usize;
        i += 1;
        if data.len() >= i + mcu_len {
            let mcu = &data[i..i + mcu_len];
            let mcu = if !mcu.is_empty() && mcu[mcu.len() - 1] == 0 {
                &mcu[..mcu.len() - 1]
            } else {
                mcu
            };
            std::str::from_utf8(mcu).ok().map(|s| s.to_string())
        } else {
            None
        }
    } else {
        None
    };

    Some((version, mcu_version))
}

/// Parse installed apps from APDU response
fn parse_installed_apps(data: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut apps = Vec::new();

    if data.is_empty() || data[0] != 0x01 {
        return apps;
    }

    let mut i = 1;
    while i < data.len() {
        if data.len() < i + 1 + 2 + 2 + 32 + 32 + 1 {
            break;
        }

        let len = data[i] as usize;
        i += 1;
        i += 2; // blocks
        i += 2; // flags
        i += 32; // hash_code_data
        let hash = data[i..i + 32].to_vec();
        i += 32;
        let name_len = data[i] as usize;
        i += 1;

        if data.len() < i + name_len || len != name_len + 70 {
            break;
        }

        if let Ok(name) = std::str::from_utf8(&data[i..i + name_len]) {
            apps.push((name.to_string(), hash));
        }
        i += name_len;
    }

    apps
}

/// Get device information.
///
/// This attempts to connect to a previously authorized device and query its info.
#[wasm_bindgen]
pub async fn get_device_info() -> Result<JsValue, JsValue> {
    // Try to get existing transport or connect to authorized device
    let transport = TRANSPORT.with(|t| t.borrow().clone());

    let transport = match transport {
        Some(t) if t.is_device_open() => t,
        _ => {
            // Try to connect to an already-authorized device
            match WebHidTransport::connect_authorized().await {
                Ok(t) => {
                    let t = Rc::new(t);
                    TRANSPORT.with(|tr| {
                        *tr.borrow_mut() = Some(t.clone());
                    });
                    t
                }
                Err(_) => {
                    let info = DeviceInfo {
                        connected: false,
                        model: None,
                        version: None,
                        mcu_version: None,
                        bitcoin_installed: false,
                        bitcoin_version: None,
                        bitcoin_test_installed: false,
                        bitcoin_test_version: None,
                    };
                    return Ok(serde_wasm_bindgen::to_value(&info)?);
                }
            }
        }
    };

    // Get version info
    let cmd = APDUCommand {
        cla: GET_VERSION.cla,
        ins: GET_VERSION.ins,
        p1: GET_VERSION.p1,
        p2: GET_VERSION.p2,
        data: vec![],
    };

    let (version, mcu_version) = match transport.exchange_async(&cmd).await {
        Ok(answer) => {
            if answer.retcode() == 0x9000 {
                parse_version_info(answer.data()).unwrap_or((String::new(), None))
            } else {
                (String::new(), None)
            }
        }
        Err(_) => (String::new(), None),
    };

    // Get installed apps
    let cmd = APDUCommand {
        cla: LIST_APPS.cla,
        ins: LIST_APPS.ins,
        p1: LIST_APPS.p1,
        p2: LIST_APPS.p2,
        data: vec![],
    };

    let mut all_apps = Vec::new();

    if let Ok(answer) = transport.exchange_async(&cmd).await {
        if answer.retcode() == 0x9000 {
            all_apps.extend(parse_installed_apps(answer.data()));

            // Continue listing if there are more
            loop {
                let cmd = APDUCommand {
                    cla: CONTINUE_LIST_APPS.cla,
                    ins: CONTINUE_LIST_APPS.ins,
                    p1: CONTINUE_LIST_APPS.p1,
                    p2: CONTINUE_LIST_APPS.p2,
                    data: vec![],
                };

                if let Ok(answer) = transport.exchange_async(&cmd).await {
                    let data = answer.data();
                    if data.is_empty() {
                        break;
                    }
                    all_apps.extend(parse_installed_apps(data));
                } else {
                    break;
                }
            }
        }
    }

    // Check for Bitcoin apps
    let bitcoin_installed = all_apps
        .iter()
        .any(|(name, _)| name.to_lowercase() == "bitcoin");
    let bitcoin_test_installed = all_apps
        .iter()
        .any(|(name, _)| name.to_lowercase() == "bitcoin test");

    let info = DeviceInfo {
        connected: true,
        model: Some("Ledger".to_string()),
        version: if version.is_empty() {
            None
        } else {
            Some(version)
        },
        mcu_version,
        bitcoin_installed,
        bitcoin_version: None, // Would need API call to get version
        bitcoin_test_installed,
        bitcoin_test_version: None,
    };

    Ok(serde_wasm_bindgen::to_value(&info)?)
}

/// Open the Bitcoin app on the device.
#[wasm_bindgen]
pub async fn open_bitcoin_app(testnet: bool) -> Result<JsValue, JsValue> {
    let app_name = if testnet { "Bitcoin Test" } else { "Bitcoin" };

    let transport = TRANSPORT.with(|t| t.borrow().clone());

    let transport = match transport {
        Some(t) if t.is_device_open() => t,
        _ => {
            let result = OperationResult {
                success: false,
                message: "Device not connected. Please connect first.".to_string(),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    // Open app command
    let cmd = APDUCommand {
        cla: 0xe0,
        ins: 0xd8,
        p1: 0x00,
        p2: 0x00,
        data: app_name.as_bytes().to_vec(),
    };

    match transport.exchange_async(&cmd).await {
        Ok(answer) => {
            if answer.retcode() == 0x9000 {
                let result = OperationResult {
                    success: true,
                    message: format!("{} app opened successfully", app_name),
                };
                Ok(serde_wasm_bindgen::to_value(&result)?)
            } else {
                let result = OperationResult {
                    success: false,
                    message: format!(
                        "Failed to open {} app. Error code: 0x{:04x}",
                        app_name,
                        answer.retcode()
                    ),
                };
                Ok(serde_wasm_bindgen::to_value(&result)?)
            }
        }
        Err(e) => {
            let result = OperationResult {
                success: false,
                message: format!("Failed to open {} app: {}", app_name, e),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
    }
}

/// Install the Bitcoin app on the device.
#[wasm_bindgen]
pub async fn install_bitcoin_app(testnet: bool) -> Result<JsValue, JsValue> {
    let app_name = if testnet { "Bitcoin Test" } else { "Bitcoin" };

    // Installing apps requires WebSocket communication with Ledger's HSM
    // This is complex and requires:
    // 1. HTTP request to get app metadata from Ledger API
    // 2. WebSocket connection to Ledger's HSM
    // 3. Multiple APDU exchanges through the WebSocket
    //
    // For now, we provide a helpful message
    let result = OperationResult {
        success: false,
        message: format!(
            "Installing {} app requires WebSocket communication with Ledger's servers. \
             This feature requires additional implementation. \
             Please use the desktop CLI or GUI app for installation.",
            app_name
        ),
    };

    Ok(serde_wasm_bindgen::to_value(&result)?)
}

/// Perform genuine check on the device.
#[wasm_bindgen]
pub async fn genuine_check() -> Result<JsValue, JsValue> {
    // Genuine check requires WebSocket communication with Ledger's HSM
    // Similar to install, this needs:
    // 1. HTTP request to get firmware info
    // 2. WebSocket connection to Ledger's HSM for verification
    //
    // For now, we provide a helpful message
    let result = OperationResult {
        success: false,
        message: "Genuine check requires WebSocket communication with Ledger's servers. \
                  This feature requires additional implementation. \
                  Please use the desktop CLI or GUI app for genuine verification."
            .to_string(),
    };

    Ok(serde_wasm_bindgen::to_value(&result)?)
}

/// Log a message to the browser console.
#[wasm_bindgen]
pub fn log(message: &str) {
    web_sys::console::log_1(&JsValue::from_str(message));
}
