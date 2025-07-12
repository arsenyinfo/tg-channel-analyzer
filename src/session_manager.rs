use grammers_client::{Client, Config};
use grammers_session::Session;
use log::{error, info, warn};
use std::fs;
use std::path::Path;
use std::env;

pub struct SessionManager;

impl SessionManager {
    /// discovers all session files in the sessions directory
    pub fn discover_sessions() -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let sessions_dir = "sessions";
        if !Path::new(sessions_dir).exists() {
            return Err("sessions/ directory does not exist".into());
        }

        let mut session_files = Vec::new();

        for entry in fs::read_dir(sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(file_name) = path.file_name() {
                    if let Some(file_str) = file_name.to_str() {
                        if file_str.ends_with(".session") {
                            session_files.push(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }

        Ok(session_files)
    }

    /// validates all sessions by attempting to connect and checking authorization
    pub async fn validate_sessions() -> Result<ValidationResult, Box<dyn std::error::Error + Send + Sync>> {
        let session_files = Self::discover_sessions()?;
        
        if session_files.is_empty() {
            return Ok(ValidationResult::NoSessions);
        }

        let mut valid_sessions = Vec::new();
        let mut invalid_sessions = Vec::new();

        info!("Validating {} session files...", session_files.len());

        for session_file in session_files {
            match Self::validate_single_session(&session_file).await {
                Ok(true) => {
                    info!("✅ Session valid: {}", session_file);
                    valid_sessions.push(session_file);
                }
                Ok(false) => {
                    warn!("❌ Session invalid/unauthorized: {}", session_file);
                    invalid_sessions.push(session_file);
                }
                Err(e) => {
                    error!("❌ Session validation error for {}: {}", session_file, e);
                    invalid_sessions.push(session_file);
                }
            }
        }

        if valid_sessions.is_empty() {
            Ok(ValidationResult::AllInvalid { invalid_sessions })
        } else {
            Ok(ValidationResult::Success { 
                valid_sessions, 
                invalid_sessions 
            })
        }
    }

    /// validates a single session by attempting to connect and check authorization
    async fn validate_single_session(session_file: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        // load session
        let session = match Session::load_file(session_file) {
            Ok(session) => session,
            Err(e) => {
                warn!("Failed to load session file {}: {}", session_file, e);
                return Ok(false);
            }
        };

        // get API credentials from environment
        let api_id = env::var("TG_API_ID")
            .map_err(|_| "TG_API_ID not set in environment")?
            .parse::<i32>()
            .map_err(|_| "TG_API_ID must be a valid integer")?;
        let api_hash = env::var("TG_API_HASH")
            .map_err(|_| "TG_API_HASH not set in environment")?;

        // attempt to create client and connect
        let client = Client::connect(Config {
            session,
            api_id,
            api_hash,
            params: Default::default(),
        }).await;

        match client {
            Ok(client) => {
                // check if client is authorized
                match client.is_authorized().await {
                    Ok(is_auth) => {
                        if is_auth {
                            Ok(true)
                        } else {
                            warn!("Session {} loaded but not authorized", session_file);
                            Ok(false)
                        }
                    }
                    Err(e) => {
                        warn!("Failed to check authorization for {}: {}", session_file, e);
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                warn!("Failed to connect with session {}: {}", session_file, e);
                Ok(false)
            }
        }
    }
}

#[derive(Debug)]
pub enum ValidationResult {
    NoSessions,
    AllInvalid { invalid_sessions: Vec<String> },
    Success { 
        valid_sessions: Vec<String>, 
        invalid_sessions: Vec<String> 
    },
}

impl ValidationResult {
    /// returns true if validation was successful and at least one session is valid
    pub fn is_success(&self) -> bool {
        matches!(self, ValidationResult::Success { .. })
    }


    /// returns error message for display to user
    pub fn error_message(&self) -> Option<String> {
        match self {
            ValidationResult::NoSessions => {
                Some("No session files found in sessions/ directory.\n\nTo create sessions:\n1. Run `cargo run --bin authorize` to create a new session\n2. Place session files in the sessions/ directory".to_string())
            }
            ValidationResult::AllInvalid { invalid_sessions } => {
                Some(format!(
                    "All {} session files are invalid or unauthorized.\n\nInvalid sessions:\n{}\n\nTo fix:\n1. Delete invalid session files\n2. Run `cargo run --bin authorize` to create new sessions\n3. Ensure sessions are properly authorized",
                    invalid_sessions.len(),
                    invalid_sessions.iter().map(|s| format!("  - {}", s)).collect::<Vec<_>>().join("\n")
                ))
            }
            ValidationResult::Success { .. } => None,
        }
    }

    /// returns success message for display to user
    pub fn success_message(&self) -> Option<String> {
        match self {
            ValidationResult::Success { valid_sessions, invalid_sessions } => {
                let mut msg = format!("✅ Session validation successful! {} valid session(s) found.", valid_sessions.len());
                if !invalid_sessions.is_empty() {
                    msg.push_str(&format!("\n⚠️  {} invalid session(s) will be ignored.", invalid_sessions.len()));
                }
                Some(msg)
            }
            _ => None,
        }
    }
}