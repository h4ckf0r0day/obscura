use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct CdpRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CdpResponse {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CdpError>,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl CdpResponse {
    pub fn success(id: u64, result: serde_json::Value, session_id: Option<String>) -> Self {
        CdpResponse {
            id,
            result: Some(result),
            error: None,
            session_id,
        }
    }

    pub fn error(id: u64, code: i64, message: String, session_id: Option<String>) -> Self {
        CdpResponse {
            id,
            result: None,
            error: Some(CdpError { code, message }),
            session_id,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CdpError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct CdpEvent {
    pub method: String,
    pub params: serde_json::Value,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl CdpEvent {
    pub fn new(method: &str, params: serde_json::Value) -> Self {
        CdpEvent {
            method: method.to_string(),
            params,
            session_id: None,
        }
    }

    pub fn with_session(method: &str, params: serde_json::Value, session_id: String) -> Self {
        CdpEvent {
            method: method.to_string(),
            params,
            session_id: Some(session_id),
        }
    }
}
