use serde::Serialize;

/// Response body for `GET /health`.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
}

/// Response body for `GET /version`.
#[derive(Debug, Serialize)]
pub struct VersionResponse {
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_serializes_to_json() {
        let health = HealthResponse {
            status: "ok".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&health).unwrap(),
            r#"{"status":"ok"}"#
        );
    }

    #[test]
    fn version_response_serializes_to_json() {
        let version = VersionResponse {
            version: "0.0.1".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&version).unwrap(),
            r#"{"version":"0.0.1"}"#
        );
    }
}
