//! Web WASM bindings for Ledger Manager.
//!
//! This crate provides JavaScript-callable functions for managing Ledger devices
//! from a web browser using the WebHID API.

use gloo_net::http::Request;
use gloo_net::websocket::{futures::WebSocket, Message};
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

// Ledger API constants
const LIVE_COMMON_VERSION: &str = "34.0.0";
const PROVIDER: u32 = 1;
const BASE_API_V1_URL: &str = "https://manager.api.live.ledger.com/api";
const BASE_API_V2_URL: &str = "https://manager.api.live.ledger.com/api/v2";
const BASE_SOCKET_URL: &str = "wss://scriptrunner.api.live.ledger.com/update";

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
    pub target_id: Option<u32>,
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

// API response types
#[derive(Debug, Clone, Deserialize)]
struct DeviceVersion {
    id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct FirmwareInfoResponse {
    perso: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BitcoinAppInfo {
    #[serde(rename = "versionName")]
    version_name: String,
    #[allow(dead_code)]
    version: String,
    perso: String,
    #[serde(rename = "deleteKey")]
    delete_key: String,
    firmware: String,
    #[serde(rename = "firmwareKey")]
    firmware_key: String,
    hash: String,
}

// HSM WebSocket message types
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum HsmMessageData {
    Command(String),
    CommandList(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
struct HsmMessage {
    query: String,
    nonce: u32,
    data: Option<HsmMessageData>,
}

/// Check if WebHID is supported in the current browser.
#[wasm_bindgen]
pub fn check_webhid_support() -> bool {
    is_webhid_supported()
}

/// Request access to a Ledger device.
#[wasm_bindgen]
pub async fn connect_device() -> Result<JsValue, JsValue> {
    match WebHidTransport::request_device().await {
        Ok(transport) => {
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

/// Stored device info for API calls
#[derive(Clone)]
struct StoredDeviceInfo {
    target_id: u32,
    version: String,
}

thread_local! {
    static DEVICE_INFO: RefCell<Option<StoredDeviceInfo>> = const { RefCell::new(None) };
}

/// Parse device version info from APDU response
fn parse_version_info(data: &[u8]) -> Option<(u32, String, Option<String>)> {
    if data.len() < 5 {
        return None;
    }

    let target_id = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    let mut i = 4;
    let ver_len = data[i] as usize;
    i += 1;

    if data.len() < i + ver_len + 1 {
        return None;
    }

    let version = std::str::from_utf8(&data[i..i + ver_len]).ok()?.to_string();
    i += ver_len;

    let flags_len = data[i] as usize;
    i += 1 + flags_len;

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

    Some((target_id, version, mcu_version))
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

/// Query app info by hashes from Ledger API
async fn get_apps_by_hashes(hashes: Vec<Vec<u8>>) -> Result<Vec<Option<BitcoinAppInfo>>, String> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let hashes_hex: Vec<serde_json::Value> = hashes
        .into_iter()
        .map(|h| serde_json::Value::String(hex::encode(&h)))
        .collect();

    let url = format!(
        "{}/apps/hash?livecommonversion={}",
        BASE_API_V2_URL, LIVE_COMMON_VERSION
    );

    let resp = Request::post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::Value::Array(hashes_hex).to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("Failed to get apps by hash: {}", resp.status()));
    }

    resp.json().await.map_err(|e| e.to_string())
}

fn deser_apdu_command(hex_str: &str) -> Result<APDUCommand<Vec<u8>>, String> {
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    if bytes.len() < 5 {
        return Err("Invalid command".into());
    }

    let (cla, ins, p1, p2, data_len) = (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4] as usize);
    if bytes.len() != 5 + data_len {
        return Err("Invalid command".into());
    }

    Ok(APDUCommand {
        cla,
        ins,
        p1,
        p2,
        data: bytes[5..].to_vec(),
    })
}

/// Get device information.
#[wasm_bindgen]
pub async fn get_device_info() -> Result<JsValue, JsValue> {
    let transport = TRANSPORT.with(|t| t.borrow().clone());

    let transport = match transport {
        Some(t) if t.is_device_open() => t,
        _ => match WebHidTransport::connect_authorized().await {
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
                    target_id: None,
                    bitcoin_installed: false,
                    bitcoin_version: None,
                    bitcoin_test_installed: false,
                    bitcoin_test_version: None,
                };
                return Ok(serde_wasm_bindgen::to_value(&info)?);
            }
        },
    };

    // Get version info
    let cmd = APDUCommand {
        cla: GET_VERSION.cla,
        ins: GET_VERSION.ins,
        p1: GET_VERSION.p1,
        p2: GET_VERSION.p2,
        data: vec![],
    };

    let (target_id, version, mcu_version) = match transport.exchange_async(&cmd).await {
        Ok(answer) => {
            if answer.retcode() == 0x9000 {
                parse_version_info(answer.data()).unwrap_or((0, String::new(), None))
            } else {
                (0, String::new(), None)
            }
        }
        Err(_) => (0, String::new(), None),
    };

    // Store device info for later API calls
    if target_id != 0 && !version.is_empty() {
        DEVICE_INFO.with(|d| {
            *d.borrow_mut() = Some(StoredDeviceInfo {
                target_id,
                version: version.clone(),
            });
        });
    }

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

    // Find Bitcoin and Bitcoin Test apps and their hashes
    let bitcoin_app = all_apps
        .iter()
        .find(|(name, _)| name.to_lowercase() == "bitcoin");
    let bitcoin_test_app = all_apps
        .iter()
        .find(|(name, _)| name.to_lowercase() == "bitcoin test");

    let bitcoin_installed = bitcoin_app.is_some();
    let bitcoin_test_installed = bitcoin_test_app.is_some();

    // Get version info by querying API with hashes
    let mut bitcoin_version = None;
    let mut bitcoin_test_version = None;

    let mut hashes_to_query = Vec::new();
    if let Some((_, hash)) = bitcoin_app {
        hashes_to_query.push(hash.clone());
    }
    if let Some((_, hash)) = bitcoin_test_app {
        hashes_to_query.push(hash.clone());
    }

    if !hashes_to_query.is_empty() {
        if let Ok(app_infos) = get_apps_by_hashes(hashes_to_query).await {
            for app_info in app_infos.into_iter().flatten() {
                let name_lower = app_info.version_name.to_lowercase();
                if name_lower == "bitcoin" {
                    bitcoin_version = Some(app_info.version);
                } else if name_lower == "bitcoin test" {
                    bitcoin_test_version = Some(app_info.version);
                }
            }
        }
    }

    let info = DeviceInfo {
        connected: true,
        model: Some("Ledger".to_string()),
        version: if version.is_empty() {
            None
        } else {
            Some(version)
        },
        mcu_version,
        target_id: if target_id != 0 {
            Some(target_id)
        } else {
            None
        },
        bitcoin_installed,
        bitcoin_version,
        bitcoin_test_installed,
        bitcoin_test_version,
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

/// Get firmware info from Ledger API
async fn get_firmware_info(target_id: u32, version: &str) -> Result<String, String> {
    // First get device version ID
    let dev_ver_url = format!(
        "{}/get_device_version?livecommonversion={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION
    );

    let dev_ver_body = serde_json::json!({
        "provider": PROVIDER,
        "target_id": target_id,
    });

    let dev_ver_resp = Request::post(&dev_ver_url)
        .header("Content-Type", "application/json")
        .body(dev_ver_body.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !dev_ver_resp.ok() {
        return Err(format!(
            "Failed to get device version: {}",
            dev_ver_resp.status()
        ));
    }

    let device_version: DeviceVersion = dev_ver_resp.json().await.map_err(|e| e.to_string())?;

    // Now get firmware info
    let firm_url = format!(
        "{}/get_firmware_version?livecommonversion={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION
    );

    let firm_body = serde_json::json!({
        "provider": PROVIDER,
        "device_version": device_version.id,
        "version_name": version,
    });

    let firm_resp = Request::post(&firm_url)
        .header("Content-Type", "application/json")
        .body(firm_body.to_string())
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !firm_resp.ok() {
        return Err(format!(
            "Failed to get firmware info: {}",
            firm_resp.status()
        ));
    }

    let firmware_info: FirmwareInfoResponse = firm_resp.json().await.map_err(|e| e.to_string())?;
    Ok(firmware_info.perso)
}

/// Get Bitcoin app info from Ledger API
async fn get_bitcoin_app_info(
    target_id: u32,
    version: &str,
    testnet: bool,
) -> Result<BitcoinAppInfo, String> {
    let url = format!(
        "{}/apps/by-target?livecommonversion={}&provider={}&target_id={}&firmware_version_name={}",
        BASE_API_V2_URL, LIVE_COMMON_VERSION, PROVIDER, target_id, version
    );

    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;

    if !resp.ok() {
        return Err(format!("Failed to get app info: {}", resp.status()));
    }

    let apps: Vec<BitcoinAppInfo> = resp.json().await.map_err(|e| e.to_string())?;

    let app_name = if testnet { "bitcoin test" } else { "bitcoin" };
    apps.into_iter()
        .find(|app| app.version_name.to_lowercase() == app_name)
        .ok_or_else(|| format!("Bitcoin app not found for this device"))
}

/// Execute WebSocket communication with Ledger HSM
async fn query_via_websocket(transport: Rc<WebHidTransport>, url: &str) -> Result<(), String> {
    use futures::{SinkExt, StreamExt};

    let ws =
        WebSocket::open(url).map_err(|e| format!("Failed to connect to WebSocket: {:?}", e))?;
    let (mut write, mut read) = ws.split();

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let hsm_msg: HsmMessage = serde_json::from_str(&text)
                    .map_err(|e| format!("Failed to parse HSM message: {}", e))?;

                if hsm_msg.query == "exchange" {
                    let command_hex = match hsm_msg.data {
                        Some(HsmMessageData::Command(h)) => h,
                        _ => return Err("Expected single command in exchange mode".into()),
                    };
                    let command = deser_apdu_command(&command_hex)?;

                    let resp = transport
                        .exchange_async(&command)
                        .await
                        .map_err(|e| format!("APDU exchange failed: {}", e))?;

                    let response = if resp.retcode() == 0x9000 {
                        "success"
                    } else {
                        "error"
                    };
                    let resp_data = hex::encode(resp.data());

                    let ws_resp = serde_json::json!({
                        "nonce": hsm_msg.nonce,
                        "response": response,
                        "data": resp_data,
                    });

                    write
                        .send(Message::Text(ws_resp.to_string()))
                        .await
                        .map_err(|e| format!("Failed to send WebSocket message: {:?}", e))?;
                } else if hsm_msg.query == "bulk" {
                    let commands = match hsm_msg.data {
                        Some(HsmMessageData::CommandList(l)) => l,
                        _ => return Err("Expected command list in bulk mode".into()),
                    };

                    for cmd_hex in commands {
                        if cmd_hex.is_empty() {
                            continue;
                        }
                        let command = deser_apdu_command(&cmd_hex)?;
                        let _ = transport
                            .exchange_async(&command)
                            .await
                            .map_err(|e| format!("APDU exchange failed: {}", e))?;
                    }

                    let ws_resp = serde_json::json!({
                        "nonce": hsm_msg.nonce,
                        "response": "success",
                        "data": "",
                    });

                    write
                        .send(Message::Text(ws_resp.to_string()))
                        .await
                        .map_err(|e| format!("Failed to send WebSocket message: {:?}", e))?;
                } else if hsm_msg.query == "success" {
                    return Ok(());
                } else if hsm_msg.query == "error" {
                    return Err(format!("HSM returned error: {}", text));
                } else if hsm_msg.query == "warning" {
                    // Log warning but continue
                    web_sys::console::warn_1(&JsValue::from_str(&format!("HSM warning: {}", text)));
                }
            }
            Ok(Message::Bytes(_)) => {
                return Err("Unexpected binary message from WebSocket".into());
            }
            Err(e) => {
                return Err(format!("WebSocket error: {:?}", e));
            }
        }
    }

    Err("WebSocket closed unexpectedly".into())
}

/// Perform genuine check on the device.
#[wasm_bindgen]
pub async fn genuine_check() -> Result<JsValue, JsValue> {
    let transport = TRANSPORT.with(|t| t.borrow().clone());
    let device_info = DEVICE_INFO.with(|d| d.borrow().clone());

    let transport = match transport {
        Some(t) if t.is_device_open() => t,
        _ => {
            let result = OperationResult {
                success: false,
                message: "Device not connected. Please connect and get device info first."
                    .to_string(),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    let device_info = match device_info {
        Some(info) => info,
        None => {
            let result = OperationResult {
                success: false,
                message: "Device info not available. Please get device info first.".to_string(),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    // Get firmware perso from API
    let perso = match get_firmware_info(device_info.target_id, &device_info.version).await {
        Ok(p) => p,
        Err(e) => {
            let result = OperationResult {
                success: false,
                message: format!("Failed to get firmware info: {}", e),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    // Build WebSocket URL for genuine check
    let ws_url = format!(
        "{}/genuine?targetId={}&perso={}",
        BASE_SOCKET_URL,
        device_info.target_id,
        urlencoding::encode(&perso)
    );

    match query_via_websocket(transport, &ws_url).await {
        Ok(()) => {
            let result = OperationResult {
                success: true,
                message: "Device is genuine!".to_string(),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
        Err(e) => {
            let result = OperationResult {
                success: false,
                message: format!("Genuine check failed: {}", e),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
    }
}

/// Install the Bitcoin app on the device.
#[wasm_bindgen]
pub async fn install_bitcoin_app(testnet: bool) -> Result<JsValue, JsValue> {
    let app_name = if testnet { "Bitcoin Test" } else { "Bitcoin" };

    let transport = TRANSPORT.with(|t| t.borrow().clone());
    let device_info = DEVICE_INFO.with(|d| d.borrow().clone());

    let transport = match transport {
        Some(t) if t.is_device_open() => t,
        _ => {
            let result = OperationResult {
                success: false,
                message: "Device not connected. Please connect and get device info first."
                    .to_string(),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    let device_info = match device_info {
        Some(info) => info,
        None => {
            let result = OperationResult {
                success: false,
                message: "Device info not available. Please get device info first.".to_string(),
            };
            return Ok(serde_wasm_bindgen::to_value(&result)?);
        }
    };

    // Get Bitcoin app info from API
    let app_info =
        match get_bitcoin_app_info(device_info.target_id, &device_info.version, testnet).await {
            Ok(info) => info,
            Err(e) => {
                let result = OperationResult {
                    success: false,
                    message: format!("Failed to get {} app info: {}", app_name, e),
                };
                return Ok(serde_wasm_bindgen::to_value(&result)?);
            }
        };

    // Build WebSocket URL for install
    let ws_url = format!(
        "{}/install?targetId={}&perso={}&deleteKey={}&firmware={}&firmwareKey={}&hash={}",
        BASE_SOCKET_URL,
        device_info.target_id,
        urlencoding::encode(&app_info.perso),
        urlencoding::encode(&app_info.delete_key),
        urlencoding::encode(&app_info.firmware),
        urlencoding::encode(&app_info.firmware_key),
        urlencoding::encode(&app_info.hash)
    );

    match query_via_websocket(transport, &ws_url).await {
        Ok(()) => {
            let result = OperationResult {
                success: true,
                message: format!("{} app installed successfully!", app_name),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
        Err(e) => {
            let result = OperationResult {
                success: false,
                message: format!("Failed to install {} app: {}", app_name, e),
            };
            Ok(serde_wasm_bindgen::to_value(&result)?)
        }
    }
}

/// Log a message to the browser console.
#[wasm_bindgen]
pub fn log(message: &str) {
    web_sys::console::log_1(&JsValue::from_str(message));
}
