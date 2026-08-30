/// Error codes surfaced to a client before it is dropped.
///
/// Kept as explicit strings because the mobile PWA branches on them to decide
/// whether to show "turn your PC on" versus "your key is wrong".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayErrorCode {
    HostOffline,
    TokenMismatch,
    RateLimited,
    InvalidRegistration,
    ClientReplaced,
}

impl RelayErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostOffline => "HOST_OFFLINE",
            Self::TokenMismatch => "TOKEN_MISMATCH",
            Self::RateLimited => "RATE_LIMITED",
            Self::InvalidRegistration => "INVALID_REGISTRATION",
            Self::ClientReplaced => "CLIENT_REPLACED",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::HostOffline => "请先在电脑上打开 Mirror X Codex",
            Self::TokenMismatch => "连接凭据不匹配，请确认 API Key 是否正确",
            Self::RateLimited => "连接过于频繁，请稍后再试",
            Self::InvalidRegistration => "连接参数无效",
            Self::ClientReplaced => "此连接已被另一台手机或浏览器标签页接管",
        }
    }

    pub fn to_json(self) -> String {
        serde_json::json!({
            "type": "error",
            "code": self.as_str(),
            "message": self.message(),
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_json_carries_code_and_message() {
        let json = RelayErrorCode::HostOffline.to_json();
        assert!(json.contains("HOST_OFFLINE"));
        assert!(json.contains("Mirror X Codex"));
    }

    #[test]
    fn every_code_has_distinct_wire_name() {
        let codes = [
            RelayErrorCode::HostOffline,
            RelayErrorCode::TokenMismatch,
            RelayErrorCode::RateLimited,
            RelayErrorCode::InvalidRegistration,
            RelayErrorCode::ClientReplaced,
        ];
        let mut names: Vec<&str> = codes.iter().map(|code| code.as_str()).collect();
        names.sort_unstable();
        let unique = names.len();
        names.dedup();
        assert_eq!(unique, names.len());
    }
}
