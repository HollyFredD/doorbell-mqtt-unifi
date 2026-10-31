//! Error codes reported to Home Assistant.
//!
//! Number ranges:
//!   0000-0999  Common / general
//!   1000-1099  Configuration / input
//!   1100-1199  Local filesystem
//!   1200-1299  Generic path / file / validation
//!   2000-2099  MQTT
//!   3000-3099  SSH connection / auth
//!   3100-3199  SSH command execution
//!   3200-3299  SCP transfer
//!   4000-4099  Profile validation / lookup / download
//!   4100-4199  Profile upload
//!   4200-4299  Profile apply
//!   4300-4399  Remote filesystem
//!   4400-4499  SFX
//!   5000-5099  State
//!
//! Keep generic utility and validation codes reusable and let callers provide
//! context-specific messages (e.g. ERROR_FILENAME_INVALID -> "Profile image filename is invalid").

#![allow(dead_code)]

use std::fmt;

macro_rules! error_codes {
    ($( $code:literal $name:ident $msg:literal )*) => {
        $( pub const $name: i32 = $code; )*

        const TABLE: &[(i32, &str, &str)] = &[ $( ($code, stringify!($name), $msg), )* ];
    };
}

error_codes! {
    // Common / general
    0    ERROR_NONE "No error"

    // Configuration / input
    1001 ERROR_CONFIG_MISSING "Configuration file not found"
    1002 ERROR_CONFIG_INVALID "Configuration is invalid"
    1003 ERROR_INVALID_PARAMETERS "Invalid parameters"

    // Local filesystem
    1100 ERROR_LOCAL_PATH_INVALID "Local path is invalid"
    1101 ERROR_LOCAL_PATH_TOO_LONG "Local path exceeds maximum length"
    1102 ERROR_LOCAL_DIRECTORY_CREATE_FAILED "Failed to create local directory"
    1103 ERROR_LOCAL_DIRECTORY_NOT_FOUND "Local directory not found"
    1104 ERROR_LOCAL_DIRECTORY_PERMISSION_DENIED "Permission denied for local directory"
    1105 ERROR_LOCAL_FILE_CREATE_FAILED "Failed to create local file"
    1106 ERROR_LOCAL_FILE_DELETE_FAILED "Failed to delete local file"
    1107 ERROR_LOCAL_FILE_READ_FAILED "Failed to read local file"
    1108 ERROR_LOCAL_FILE_WRITE_FAILED "Failed to write local file"
    1109 ERROR_LOCAL_TEMP_DIR_CREATE_FAILED "Failed to create temporary directory"
    1110 ERROR_LOCAL_TEMP_DIR_CLEANUP_FAILED "Failed to clean up temporary directory"
    1111 ERROR_LOCAL_DISK_FULL "Local disk is full"

    // Generic path / file / validation
    1200 ERROR_VALIDATION_FAILED "Validation failed"
    1201 ERROR_PATH_INVALID "Path is invalid"
    1202 ERROR_FILENAME_INVALID "Filename is invalid"
    1203 ERROR_FILE_EXTENSION_INVALID "File extension is invalid"
    1204 ERROR_FILE_TYPE_INVALID "File type is invalid"
    1205 ERROR_FILE_NOT_FOUND "File not found"
    1206 ERROR_DIRECTORY_NOT_FOUND "Directory not found"
    1207 ERROR_REQUIRED_FILE_MISSING "Required file is missing"
    1208 ERROR_REQUIRED_DIRECTORY_MISSING "Required directory is missing"
    1209 ERROR_FILE_TOO_LARGE "File exceeds maximum allowed size"
    1210 ERROR_PATH_TOO_LONG "Path exceeds maximum length"

    // MQTT
    2001 ERROR_MQTT_CONNECTION_FAILED "MQTT connection failed"

    // SSH connection / authentication
    3001 ERROR_SSH_CONNECTION_FAILED "SSH connection failed"
    3002 ERROR_SSH_AUTH_FAILED "SSH authentication failed"

    // SSH command execution
    3100 ERROR_SSH_COMMAND_FAILED "Remote SSH command failed"
    3101 ERROR_SSH_COMMAND_TIMEOUT "Remote SSH command timed out"
    3102 ERROR_SSH_COMMAND_PERMISSION_DENIED "Permission denied while executing SSH command"
    3103 ERROR_SSH_COMMAND_NOT_FOUND "Remote command not found"
    3104 ERROR_SSH_COMMAND_INVALID_RESPONSE "Unexpected response from remote command"

    // SCP transfer
    3200 ERROR_SCP_TRANSFER_FAILED "SCP file transfer failed"
    3201 ERROR_SCP_TRANSFER_TIMEOUT "SCP file transfer timed out"
    3202 ERROR_SCP_PERMISSION_DENIED "Permission denied during SCP transfer"
    3203 ERROR_SCP_VERIFY_FAILED "SCP transfer verification failed"
    3204 ERROR_SCP_REMOTE_DISK_FULL "Remote disk is full during SCP transfer"

    // Profile validation / lookup / download
    4001 ERROR_PROFILE_NOT_FOUND "Profile folder not found"
    4002 ERROR_PROFILE_INVALID "Profile is invalid"
    4003 ERROR_PROFILE_DOWNLOAD_FAILED "Profile download failed"

    // Profile upload
    4100 ERROR_PROFILE_UPLOAD_FAILED "Profile upload failed"
    4101 ERROR_PROFILE_UPLOAD_CONNECT_FAILED "Failed to connect for profile upload"
    4102 ERROR_PROFILE_UPLOAD_PERMISSION_DENIED "Permission denied during profile upload"
    4103 ERROR_PROFILE_UPLOAD_TRANSFER_FAILED "File transfer failed during profile upload"
    4104 ERROR_PROFILE_UPLOAD_VERIFY_FAILED "Uploaded profile verification failed"
    4105 ERROR_PROFILE_UPLOAD_TIMEOUT "Profile upload timed out"

    // Profile apply
    4200 ERROR_PROFILE_APPLY_FAILED "Failed to apply profile"
    4201 ERROR_PROFILE_APPLY_COMMAND_FAILED "Profile apply command failed"
    4202 ERROR_PROFILE_APPLY_TIMEOUT "Timed out while applying profile"
    4203 ERROR_PROFILE_APPLY_INVALID_STATE "Doorbell reported invalid state during apply"
    4204 ERROR_PROFILE_APPLY_VERIFY_FAILED "Profile apply verification failed"
    4205 ERROR_PROFILE_APPLY_PERMISSION_DENIED "Permission denied while applying profile"
    4206 ERROR_PROFILE_APPLY_FILE_OPERATION_FAILED "File operation failed while applying profile"
    4212 ERROR_PROFILE_APPLY_RESTART_FAILED "Failed to restart services after applying profile"
    4213 ERROR_PROFILE_APPLY_MISSING_TMP_FILES "Temporary uploaded files missing during apply"

    // Remote filesystem
    4300 ERROR_REMOTE_PATH_INVALID "Remote path is invalid or missing"
    4301 ERROR_REMOTE_DISK_FULL "Remote device is out of storage space"

    // SFX
    4400 ERROR_SFX_FAILED "SFX operation failed"
    4401 ERROR_SFX_PRESET_NOT_FOUND "SFX preset not found"
    4402 ERROR_SFX_VOLUME_INVALID "SFX volume is invalid"
    4403 ERROR_SFX_UPLOAD_FAILED "SFX upload failed"
    4404 ERROR_SFX_PLAY_COMMAND_FAILED "SFX play command failed"

    // State
    5000 ERROR_STATE_FAILED "State operation failed"
    5001 ERROR_STATE_SAVE_FAILED "Failed to save state file"
    5002 ERROR_STATE_LOAD_FAILED "Failed to load state file"
    5003 ERROR_STATE_FILE_NOT_FOUND "State file not found"
    5004 ERROR_STATE_FILE_INVALID "State file is invalid"
    5005 ERROR_STATE_FILE_READ_FAILED "Failed to read state file"
    5006 ERROR_STATE_FILE_WRITE_FAILED "Failed to write state file"
    5007 ERROR_STATE_PARSE_FAILED "Failed to parse state JSON"
    5008 ERROR_STATE_HASH_FAILED "Failed to compute state hash"
    5009 ERROR_STATE_COMPARE_FAILED "State comparison failed"
    5010 ERROR_STATE_VALIDATION_UNAVAILABLE "State validation unavailable"
    5011 ERROR_STATE_HASH_MISSING "State hash missing for comparison"
}

fn find(code: i32) -> Option<&'static (i32, &'static str, &'static str)> {
    TABLE.iter().find(|e| e.0 == code)
}

pub fn code_name(code: i32) -> &'static str {
    find(code).map_or("ERR_UNKNOWN", |e| e.1)
}

pub fn default_message(code: i32) -> &'static str {
    find(code).map_or("Unknown error", |e| e.2)
}

/// An error code plus a context-specific message, as published on `last_error`.
#[derive(Debug, Clone, PartialEq)]
pub struct AppError {
    pub code: i32,
    pub message: String,
}

impl AppError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        AppError {
            code,
            message: message.into(),
        }
    }

    /// Error carrying the code's default message.
    pub fn from_code(code: i32) -> Self {
        AppError::new(code, default_message(code))
    }

    /// Keeps the code, prefixes the message: "<context>: <inner message>".
    pub fn wrap(self, context: impl fmt::Display) -> Self {
        AppError {
            code: self.code,
            message: format!("{context}: {}", self.message),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, code_name(self.code))
    }
}

pub type AppResult<T = ()> = Result<T, AppError>;
