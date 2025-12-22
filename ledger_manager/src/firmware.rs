//! Firmware update functionality for Ledger devices.
//!
//! This module provides functions to check for firmware updates and perform
//! the complete firmware update flow including OSU installation, MCU/bootloader
//! flashing, and final firmware installation.

use crate::{
    DeviceInfo, HsmMessage, HsmMessageData, StatusCode, BASE_API_V1_URL, BASE_SOCKET_URL,
    LIVE_COMMON_VERSION, PROVIDER,
};
use form_urlencoded::Serializer as UrlSerializer;
use ledger_transport_hidapi::TransportNativeHID;
use serde_derive::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

/// Maximum number of MCU/bootloader flash iterations before giving up.
pub const MAX_MCU_ITERATIONS: u32 = 5;

// ============================================================================
// Types
// ============================================================================

/// OSU (Operating System Updater) firmware metadata from Ledger API.
///
/// The OSU is the first step in a firmware update - it prepares the device
/// for receiving the final firmware.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OsuFirmware {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub perso: String,
    pub firmware: String,
    pub firmware_key: String,
    pub hash: String,
    #[serde(default)]
    pub date_creation: String,
    #[serde(default)]
    pub date_last_modified: String,
    #[serde(default)]
    pub device_versions: Vec<i64>,
    #[serde(default)]
    pub providers: Vec<i64>,
    /// ID of the final firmware that will be installed after this OSU.
    pub next_se_firmware_final_version: i64,
    /// IDs of firmware versions that can upgrade to this OSU.
    #[serde(default)]
    pub previous_se_firmware_final_version: Vec<i64>,
}

/// Final firmware metadata from Ledger API.
///
/// This is the actual firmware that runs on the device after the update.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FinalFirmware {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    pub perso: String,
    pub firmware: String,
    pub firmware_key: String,
    pub hash: String,
    /// Firmware version string (e.g., "2.1.0").
    pub version: String,
    /// ID of the SE firmware this belongs to.
    #[serde(default)]
    pub se_firmware: i64,
    /// OSU versions that can install this firmware.
    #[serde(default)]
    pub osu_versions: Vec<OsuFirmware>,
    /// IDs of compatible MCU versions.
    #[serde(default)]
    pub mcu_versions: Vec<i64>,
    /// IDs of application versions compatible with this firmware.
    #[serde(default)]
    pub application_versions: Vec<i64>,
    /// Size of the firmware in bytes.
    #[serde(default)]
    pub bytes: Option<u64>,
    #[serde(default)]
    pub date_creation: String,
    #[serde(default)]
    pub date_last_modified: String,
    #[serde(default)]
    pub device_versions: Vec<i64>,
    #[serde(default)]
    pub providers: Vec<i64>,
}

/// MCU (Microcontroller Unit) version metadata from Ledger API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McuVersion {
    pub id: i64,
    /// MCU identifier.
    pub mcu: i64,
    /// MCU version name (e.g., "1.12").
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub providers: Vec<i64>,
    /// The bootloader version required to flash this MCU.
    pub from_bootloader_version: String,
    #[serde(default)]
    pub device_versions: Vec<i64>,
    #[serde(default)]
    pub se_firmware_final_versions: Vec<i64>,
    #[serde(default)]
    pub date_creation: String,
    #[serde(default)]
    pub date_last_modified: String,
}

/// Context containing all information needed to perform a firmware update.
#[derive(Debug, Clone)]
pub struct FirmwareUpdateContext {
    /// OSU firmware to install first.
    pub osu: OsuFirmware,
    /// Final firmware to install after OSU.
    pub final_firmware: FinalFirmware,
    /// Whether MCU needs to be flashed (device will go through bootloader).
    pub should_flash_mcu: bool,
}

/// Current phase of the firmware update process.
#[derive(Debug, Clone, PartialEq)]
pub enum FirmwareUpdatePhase {
    /// Checking for available firmware updates.
    CheckingForUpdates,
    /// Downloading firmware metadata from API.
    DownloadingMetadata,
    /// Installing OSU (Operating System Updater).
    InstallingOsu {
        /// Progress from 0.0 to 1.0.
        progress: f32,
    },
    /// Waiting for device to reboot into bootloader mode.
    WaitingForBootloader,
    /// Flashing MCU or bootloader firmware.
    FlashingMcu {
        /// Current flash iteration (1 to MAX_MCU_ITERATIONS).
        iteration: u32,
        /// Progress from 0.0 to 1.0.
        progress: f32,
    },
    /// Installing final firmware.
    InstallingFinalFirmware {
        /// Progress from 0.0 to 1.0.
        progress: f32,
    },
    /// Update completed successfully.
    Completed,
    /// Update failed with an error.
    Failed {
        /// Error description.
        error: String,
    },
}

/// WebSocket event types for tracking progress during firmware operations.
#[derive(Debug, Clone)]
pub enum WebSocketEvent {
    /// WebSocket connection opened.
    Opened,
    /// Bulk operation progress update.
    BulkProgress {
        /// Progress from 0.0 to 1.0.
        progress: f32,
        /// Current command index.
        index: usize,
        /// Total number of commands.
        total: usize,
    },
    /// APDU exchange completed.
    Exchange {
        /// Message nonce.
        nonce: u32,
        /// Response status code.
        status: u16,
    },
    /// HSM operation succeeded.
    Success,
    /// HSM operation failed.
    Error(String),
    /// HSM sent a warning (non-fatal).
    Warning(String),
    /// WebSocket connection closed.
    Closed,
}

/// Errors that can occur during firmware update operations.
#[derive(Debug, Clone)]
pub enum FirmwareUpdateError {
    /// Firmware is already up to date.
    AlreadyUpToDate,
    /// Device must be on dashboard (not in bootloader/OSU mode).
    DeviceNotOnDashboard,
    /// Device must be in OSU mode to install final firmware.
    DeviceNotInOsuMode,
    /// Device is not onboarded (no PIN set).
    DeviceNotOnboarded,
    /// Not enough storage space on device.
    NotEnoughSpace,
    /// User refused the update on the device.
    UserRefused,
    /// Too many MCU/bootloader flash iterations (exceeded MAX_MCU_ITERATIONS).
    TooManyMcuIterations,
    /// Device disconnected during update.
    DeviceDisconnected,
    /// API request failed.
    ApiError(String),
    /// WebSocket communication error.
    WebSocketError(String),
    /// Unknown or incompatible MCU version.
    UnknownMcu,
    /// No firmware update available for this device.
    NoUpdateAvailable,
    /// Generic error.
    Other(String),
}

impl fmt::Display for FirmwareUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FirmwareUpdateError::AlreadyUpToDate => write!(f, "Firmware is already up to date"),
            FirmwareUpdateError::DeviceNotOnDashboard => {
                write!(f, "Device must be on dashboard to start update")
            }
            FirmwareUpdateError::DeviceNotInOsuMode => {
                write!(f, "Device must be in OSU mode to install final firmware")
            }
            FirmwareUpdateError::DeviceNotOnboarded => {
                write!(
                    f,
                    "Device is not onboarded. Please complete device setup first."
                )
            }
            FirmwareUpdateError::NotEnoughSpace => {
                write!(f, "Not enough space on device. Please uninstall some apps.")
            }
            FirmwareUpdateError::UserRefused => {
                write!(f, "Update was cancelled on the device")
            }
            FirmwareUpdateError::TooManyMcuIterations => {
                write!(f, "MCU flash failed after {} attempts", MAX_MCU_ITERATIONS)
            }
            FirmwareUpdateError::DeviceDisconnected => {
                write!(f, "Device disconnected during update")
            }
            FirmwareUpdateError::ApiError(e) => write!(f, "API error: {}", e),
            FirmwareUpdateError::WebSocketError(e) => write!(f, "WebSocket error: {}", e),
            FirmwareUpdateError::UnknownMcu => write!(f, "Unknown or incompatible MCU version"),
            FirmwareUpdateError::NoUpdateAvailable => {
                write!(f, "No firmware update available for this device")
            }
            FirmwareUpdateError::Other(e) => write!(f, "{}", e),
        }
    }
}

impl Error for FirmwareUpdateError {}

impl From<Box<dyn Error>> for FirmwareUpdateError {
    fn from(e: Box<dyn Error>) -> Self {
        FirmwareUpdateError::Other(e.to_string())
    }
}

// ============================================================================
// API Response Types (internal)
// ============================================================================

/// Response from /get_device_version endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceVersionResponse {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    pub target_id: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub device: i64,
    #[serde(default)]
    pub providers: Vec<i64>,
    #[serde(default)]
    pub mcu_versions: Vec<i64>,
    #[serde(default)]
    pub se_firmware_final_versions: Vec<i64>,
    #[serde(default)]
    pub osu_versions: Vec<i64>,
}

/// Response from /get_latest_firmware endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct LatestFirmwareResponse {
    pub result: String,
    #[serde(default)]
    pub se_firmware_osu_version: Option<OsuFirmware>,
}

// ============================================================================
// API Functions
// ============================================================================

/// Get the device version info from the Ledger API.
pub fn get_device_version_from_api(
    target_id: u32,
) -> Result<DeviceVersionResponse, FirmwareUpdateError> {
    let url = format!(
        "{}/get_device_version?livecommonversion={}&provider={}&target_id={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION, PROVIDER, target_id
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    resp.json::<DeviceVersionResponse>()
        .map_err(|e| FirmwareUpdateError::ApiError(format!("Failed to parse response: {}", e)))
}

/// Get the latest available firmware for a device.
pub fn get_latest_firmware_from_api(
    current_version: i64,
    device_version: i64,
) -> Result<Option<OsuFirmware>, FirmwareUpdateError> {
    let url = format!(
        "{}/get_latest_firmware?livecommonversion={}&current_se_firmware_final_version={}&device_version={}&provider={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION, current_version, device_version, PROVIDER
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    let latest: LatestFirmwareResponse = resp
        .json()
        .map_err(|e| FirmwareUpdateError::ApiError(format!("Failed to parse response: {}", e)))?;

    Ok(latest.se_firmware_osu_version)
}

/// Get OSU firmware metadata by version name.
pub fn get_osu_firmware(
    device_version_id: i64,
    version_name: &str,
) -> Result<OsuFirmware, FirmwareUpdateError> {
    // OSU version name has "-osu" suffix
    let osu_version_name = if version_name.ends_with("-osu") {
        version_name.to_string()
    } else {
        format!("{}-osu", version_name)
    };

    let url = format!(
        "{}/get_osu_version?livecommonversion={}&device_version={}&version_name={}&provider={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION, device_version_id, osu_version_name, PROVIDER
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    resp.json::<OsuFirmware>()
        .map_err(|e| FirmwareUpdateError::ApiError(format!("Failed to parse OSU firmware: {}", e)))
}

/// Get final firmware by ID.
pub fn get_final_firmware_by_id(id: i64) -> Result<FinalFirmware, FirmwareUpdateError> {
    let url = format!(
        "{}/firmware_final_versions/{}?livecommonversion={}",
        BASE_API_V1_URL, id, LIVE_COMMON_VERSION
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    resp.json::<FinalFirmware>().map_err(|e| {
        FirmwareUpdateError::ApiError(format!("Failed to parse final firmware: {}", e))
    })
}

/// Get all MCU versions from the API.
pub fn get_all_mcu_versions() -> Result<Vec<McuVersion>, FirmwareUpdateError> {
    let url = format!(
        "{}/mcu_versions?livecommonversion={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    resp.json::<Vec<McuVersion>>()
        .map_err(|e| FirmwareUpdateError::ApiError(format!("Failed to parse MCU versions: {}", e)))
}

/// Get the current firmware version ID for a device.
pub fn get_current_firmware_version_id(
    device_version_id: i64,
    version_name: &str,
) -> Result<i64, FirmwareUpdateError> {
    let url = format!(
        "{}/get_firmware_version?livecommonversion={}&device_version={}&version_name={}&provider={}",
        BASE_API_V1_URL, LIVE_COMMON_VERSION, device_version_id, version_name, PROVIDER
    );

    let resp = minreq::get(&url)
        .send()
        .map_err(|e| FirmwareUpdateError::ApiError(e.to_string()))?;

    if resp.status_code != 200 {
        return Err(FirmwareUpdateError::ApiError(format!(
            "HTTP {}: {}",
            resp.status_code,
            resp.as_str().unwrap_or("Unknown error")
        )));
    }

    #[derive(Deserialize)]
    struct FirmwareVersionResponse {
        id: i64,
    }

    let fw: FirmwareVersionResponse = resp.json().map_err(|e| {
        FirmwareUpdateError::ApiError(format!("Failed to parse firmware version: {}", e))
    })?;

    Ok(fw.id)
}

// ============================================================================
// Main Firmware Update Functions
// ============================================================================

/// Check if a firmware update is available for the device.
///
/// Returns `Some(FirmwareUpdateContext)` if an update is available,
/// `None` if the firmware is already up to date.
pub fn get_latest_firmware_for_device(
    device_info: &DeviceInfo,
) -> Result<Option<FirmwareUpdateContext>, FirmwareUpdateError> {
    // 1. Get device version from API
    let device_version = get_device_version_from_api(device_info.target_id)?;

    // 2. Get current firmware version ID
    let current_fw_id = get_current_firmware_version_id(device_version.id, &device_info.version)?;

    // 3. Check for latest firmware
    let osu = match get_latest_firmware_from_api(current_fw_id, device_version.id)? {
        Some(osu) => osu,
        None => return Ok(None), // Already up to date
    };

    // 4. Get final firmware info
    let final_firmware = get_final_firmware_by_id(osu.next_se_firmware_final_version)?;

    // 5. Check if MCU needs flashing
    let mcus = get_all_mcu_versions()?;
    let current_mcu_name = device_info.mcu_version.as_deref().unwrap_or("");

    // Find current MCU version
    let current_mcu = mcus.iter().find(|m| m.name == current_mcu_name);

    // MCU needs flashing if current MCU is not in the list of compatible MCUs
    let should_flash_mcu = match current_mcu {
        Some(mcu) => !final_firmware.mcu_versions.contains(&mcu.id),
        None => true, // Unknown MCU, assume it needs flashing
    };

    Ok(Some(FirmwareUpdateContext {
        osu,
        final_firmware,
        should_flash_mcu,
    }))
}

/// Execute a WebSocket operation with progress callbacks.
///
/// This is an enhanced version of `query_via_websocket` that reports progress
/// for bulk operations.
pub fn query_via_websocket_with_progress<F>(
    ledger_api: &TransportNativeHID,
    url: &str,
    event_callback: F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(WebSocketEvent),
{
    use tungstenite::{connect, Message};

    let (mut socket, _) = connect(url)
        .map_err(|e| FirmwareUpdateError::WebSocketError(format!("Failed to connect: {}", e)))?;

    event_callback(WebSocketEvent::Opened);

    loop {
        let msg = socket
            .read()
            .map_err(|e| FirmwareUpdateError::WebSocketError(format!("Read error: {}", e)))?;

        match msg {
            Message::Text(text) => {
                let hsm_msg: HsmMessage = serde_json::from_str(&text).map_err(|e| {
                    FirmwareUpdateError::WebSocketError(format!("Parse error: {}", e))
                })?;

                if hsm_msg.query == "exchange" {
                    let command_hex = match hsm_msg.data {
                        Some(HsmMessageData::Command(h)) => h,
                        _ => {
                            return Err(FirmwareUpdateError::WebSocketError(
                                "Expected single command in exchange mode".into(),
                            ))
                        }
                    };

                    let command = crate::deser_apdu_command(&command_hex).map_err(|e| {
                        FirmwareUpdateError::WebSocketError(format!("APDU parse error: {}", e))
                    })?;

                    let resp = ledger_api
                        .exchange(&command)
                        .map_err(|_| FirmwareUpdateError::DeviceDisconnected)?;

                    // Map specific error codes
                    let status = resp.retcode();
                    if status == 0x6985 || status == 0x5501 {
                        return Err(FirmwareUpdateError::UserRefused);
                    } else if status == 0x6a84 || status == 0x5103 {
                        return Err(FirmwareUpdateError::NotEnoughSpace);
                    } else if status == 0x6d07 || status == 0x6611 {
                        return Err(FirmwareUpdateError::DeviceNotOnboarded);
                    }

                    event_callback(WebSocketEvent::Exchange {
                        nonce: hsm_msg.nonce,
                        status,
                    });

                    let response = if status == StatusCode::OK as u16 {
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

                    socket
                        .send(Message::Text(serde_json::to_string(&ws_resp).unwrap()))
                        .map_err(|e| {
                            FirmwareUpdateError::WebSocketError(format!("Send error: {}", e))
                        })?;
                } else if hsm_msg.query == "bulk" {
                    let commands = match hsm_msg.data {
                        Some(HsmMessageData::CommandList(l)) => l,
                        _ => {
                            return Err(FirmwareUpdateError::WebSocketError(
                                "Expected command list in bulk mode".into(),
                            ))
                        }
                    };

                    let total = commands.len();
                    for (index, cmd_hex) in commands.iter().enumerate() {
                        if cmd_hex.is_empty() {
                            continue;
                        }

                        // Report progress
                        let progress = (index as f32 + 1.0) / total as f32;
                        event_callback(WebSocketEvent::BulkProgress {
                            progress,
                            index,
                            total,
                        });

                        let command = crate::deser_apdu_command(cmd_hex).map_err(|e| {
                            FirmwareUpdateError::WebSocketError(format!("APDU parse error: {}", e))
                        })?;

                        let _ = ledger_api
                            .exchange(&command)
                            .map_err(|_| FirmwareUpdateError::DeviceDisconnected)?;
                    }

                    let ws_resp = serde_json::json!({
                        "nonce": hsm_msg.nonce,
                        "response": "success",
                        "data": "",
                    });

                    socket
                        .send(Message::Text(serde_json::to_string(&ws_resp).unwrap()))
                        .map_err(|e| {
                            FirmwareUpdateError::WebSocketError(format!("Send error: {}", e))
                        })?;
                } else if hsm_msg.query == "success" {
                    event_callback(WebSocketEvent::Success);
                    return Ok(());
                } else if hsm_msg.query == "error" {
                    event_callback(WebSocketEvent::Error(text.clone()));
                    return Err(FirmwareUpdateError::WebSocketError(format!(
                        "HSM error: {}",
                        text
                    )));
                } else if hsm_msg.query == "warning" {
                    event_callback(WebSocketEvent::Warning(text));
                }
            }
            Message::Close(_) => {
                event_callback(WebSocketEvent::Closed);
                return Err(FirmwareUpdateError::WebSocketError(
                    "WebSocket closed unexpectedly".into(),
                ));
            }
            _ => {}
        }
    }
}

/// Install OSU firmware via WebSocket.
fn install_osu<F>(
    ledger_api: &TransportNativeHID,
    device_info: &DeviceInfo,
    osu: &OsuFirmware,
    progress_callback: &F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(FirmwareUpdatePhase),
{
    // Device must be on dashboard (not in bootloader)
    if device_info.is_bootloader {
        return Err(FirmwareUpdateError::DeviceNotOnDashboard);
    }

    let url = UrlSerializer::new(format!("{}/install?", BASE_SOCKET_URL))
        .append_pair("targetId", &device_info.target_id.to_string())
        .append_pair("firmware", &osu.firmware)
        .append_pair("firmwareKey", &osu.firmware_key)
        .append_pair("perso", &osu.perso)
        .append_pair("livecommonversion", LIVE_COMMON_VERSION)
        .finish();

    query_via_websocket_with_progress(ledger_api, &url, |event| {
        if let WebSocketEvent::BulkProgress { progress, .. } = event {
            progress_callback(FirmwareUpdatePhase::InstallingOsu { progress });
        }
    })
}

/// Flash MCU or bootloader via WebSocket.
fn flash_mcu<F>(
    ledger_api: &TransportNativeHID,
    device_info: &DeviceInfo,
    mcu_version: &str,
    iteration: u32,
    progress_callback: &F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(FirmwareUpdatePhase),
{
    let url = UrlSerializer::new(format!("{}/mcu?", BASE_SOCKET_URL))
        .append_pair("targetId", &device_info.target_id.to_string())
        .append_pair("version", mcu_version)
        .append_pair("livecommonversion", LIVE_COMMON_VERSION)
        .finish();

    query_via_websocket_with_progress(ledger_api, &url, |event| {
        if let WebSocketEvent::BulkProgress { progress, .. } = event {
            progress_callback(FirmwareUpdatePhase::FlashingMcu {
                iteration,
                progress,
            });
        }
    })
}

/// Install final firmware via WebSocket.
fn install_final_firmware<F>(
    ledger_api: &TransportNativeHID,
    device_info: &DeviceInfo,
    firmware: &FinalFirmware,
    progress_callback: &F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(FirmwareUpdatePhase),
{
    // Device must be in OSU mode for final firmware
    if !device_info.version.contains("-osu") {
        return Err(FirmwareUpdateError::DeviceNotInOsuMode);
    }

    let url = UrlSerializer::new(format!("{}/install?", BASE_SOCKET_URL))
        .append_pair("targetId", &device_info.target_id.to_string())
        .append_pair("firmware", &firmware.firmware)
        .append_pair("firmwareKey", &firmware.firmware_key)
        .append_pair("perso", &firmware.perso)
        .append_pair("livecommonversion", LIVE_COMMON_VERSION)
        .finish();

    query_via_websocket_with_progress(ledger_api, &url, |event| {
        if let WebSocketEvent::BulkProgress { progress, .. } = event {
            progress_callback(FirmwareUpdatePhase::InstallingFinalFirmware { progress });
        }
    })
}

/// Find the best MCU version to flash based on current device state and target firmware.
fn find_best_mcu_version(
    mcus: &[McuVersion],
    device_info: &DeviceInfo,
    target_mcu_ids: &[i64],
) -> Result<String, FirmwareUpdateError> {
    // Get current bootloader version (majMin from device info)
    // In bootloader mode, we can check which MCU can be flashed

    // Find MCUs that are compatible with the target firmware
    let compatible_mcus: Vec<&McuVersion> = mcus
        .iter()
        .filter(|m| target_mcu_ids.contains(&m.id))
        .collect();

    if compatible_mcus.is_empty() {
        return Err(FirmwareUpdateError::UnknownMcu);
    }

    // If in bootloader, check bootloader compatibility
    if device_info.is_bootloader {
        // The from_bootloader_version field indicates which bootloader version is needed
        // to flash this MCU. We need to find one that matches.

        // For now, return the first compatible MCU version name
        // In a more sophisticated implementation, we'd check bootloader compatibility
        return Ok(compatible_mcus[0].name.clone());
    }

    // Return first compatible MCU
    Ok(compatible_mcus[0].name.clone())
}

/// Execute the complete firmware update process.
///
/// This function performs the full firmware update flow:
/// 1. Install OSU (Operating System Updater)
/// 2. Flash MCU/bootloader if needed (device reboots to bootloader)
/// 3. Install final firmware
///
/// The `progress_callback` is called with the current phase throughout the process.
pub fn update_firmware<F>(
    ledger_api: &TransportNativeHID,
    context: &FirmwareUpdateContext,
    progress_callback: F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(FirmwareUpdatePhase),
{
    // Get current device info
    let device_info = DeviceInfo::new(ledger_api)
        .map_err(|e| FirmwareUpdateError::Other(format!("Failed to get device info: {}", e)))?;

    // Phase 1: Install OSU
    progress_callback(FirmwareUpdatePhase::InstallingOsu { progress: 0.0 });
    install_osu(ledger_api, &device_info, &context.osu, &progress_callback)?;

    // Phase 2: MCU/Bootloader (if needed)
    if context.should_flash_mcu {
        progress_callback(FirmwareUpdatePhase::WaitingForBootloader);

        // Device will reboot after OSU installation
        // Wait a bit for reboot
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Get MCU versions for flashing
        let mcus = get_all_mcu_versions()?;

        // Flash MCU/Bootloader (loop up to MAX_MCU_ITERATIONS times)
        for iteration in 1..=MAX_MCU_ITERATIONS {
            progress_callback(FirmwareUpdatePhase::FlashingMcu {
                iteration,
                progress: 0.0,
            });

            // Re-get device info after reboot
            let current_info = DeviceInfo::new(ledger_api).map_err(|e| {
                FirmwareUpdateError::Other(format!("Failed to get device info: {}", e))
            })?;

            // If no longer in bootloader, we're done with MCU flashing
            if !current_info.is_bootloader {
                break;
            }

            // Find and flash the appropriate MCU
            let mcu_version =
                find_best_mcu_version(&mcus, &current_info, &context.final_firmware.mcu_versions)?;

            flash_mcu(
                ledger_api,
                &current_info,
                &mcu_version,
                iteration,
                &progress_callback,
            )?;

            // Wait for device to process
            std::thread::sleep(std::time::Duration::from_secs(2));

            if iteration == MAX_MCU_ITERATIONS {
                // Check one more time if we exited bootloader
                let check_info = DeviceInfo::new(ledger_api)
                    .map_err(|_| FirmwareUpdateError::TooManyMcuIterations)?;

                if check_info.is_bootloader {
                    return Err(FirmwareUpdateError::TooManyMcuIterations);
                }
            }
        }
    }

    // Phase 3: Install Final Firmware
    // Re-get device info (should be in OSU mode now)
    let osu_device_info = DeviceInfo::new(ledger_api)
        .map_err(|e| FirmwareUpdateError::Other(format!("Failed to get device info: {}", e)))?;

    progress_callback(FirmwareUpdatePhase::InstallingFinalFirmware { progress: 0.0 });
    install_final_firmware(
        ledger_api,
        &osu_device_info,
        &context.final_firmware,
        &progress_callback,
    )?;

    progress_callback(FirmwareUpdatePhase::Completed);
    Ok(())
}

/// Repair a device that is stuck in bootloader mode.
///
/// This function attempts to recover a device by flashing the appropriate
/// MCU/bootloader and then installing firmware.
pub fn repair_device_in_bootloader<F>(
    ledger_api: &TransportNativeHID,
    progress_callback: F,
) -> Result<(), FirmwareUpdateError>
where
    F: Fn(FirmwareUpdatePhase),
{
    // Get device info - must be in bootloader mode
    let device_info = DeviceInfo::new(ledger_api)
        .map_err(|e| FirmwareUpdateError::Other(format!("Failed to get device info: {}", e)))?;

    if !device_info.is_bootloader {
        return Err(FirmwareUpdateError::Other(
            "Device is not in bootloader mode. Use normal update instead.".into(),
        ));
    }

    // Get device version from API using SE target ID
    let device_version = get_device_version_from_api(device_info.se_target_id)?;

    // Get MCU versions
    let mcus = get_all_mcu_versions()?;

    // Try to find latest firmware for this device
    // In repair mode, we'll try to get any compatible firmware
    let final_firmware = if !device_version.se_firmware_final_versions.is_empty() {
        // Get the first available final firmware
        let fw_id = device_version.se_firmware_final_versions[0];
        get_final_firmware_by_id(fw_id)?
    } else {
        return Err(FirmwareUpdateError::NoUpdateAvailable);
    };

    // Flash MCU/Bootloader until device exits bootloader
    for iteration in 1..=MAX_MCU_ITERATIONS {
        progress_callback(FirmwareUpdatePhase::FlashingMcu {
            iteration,
            progress: 0.0,
        });

        // Re-check device state
        let current_info = DeviceInfo::new(ledger_api)
            .map_err(|e| FirmwareUpdateError::Other(format!("Failed to get device info: {}", e)))?;

        if !current_info.is_bootloader {
            // Device exited bootloader, now install final firmware
            progress_callback(FirmwareUpdatePhase::InstallingFinalFirmware { progress: 0.0 });
            install_final_firmware(
                ledger_api,
                &current_info,
                &final_firmware,
                &progress_callback,
            )?;
            progress_callback(FirmwareUpdatePhase::Completed);
            return Ok(());
        }

        // Find and flash MCU
        let mcu_version =
            find_best_mcu_version(&mcus, &current_info, &final_firmware.mcu_versions)?;

        flash_mcu(
            ledger_api,
            &current_info,
            &mcu_version,
            iteration,
            &progress_callback,
        )?;

        // Wait for device to process
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    Err(FirmwareUpdateError::TooManyMcuIterations)
}
