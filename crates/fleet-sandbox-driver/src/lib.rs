//! In-container sandbox driver library: provision workspace + drive harness over ACP.

pub mod acp;
pub mod bus;
pub mod handshake;
pub mod harness;
pub mod job;
pub mod opencode_config;
pub mod pi_config;
pub mod policy;
pub mod provision;

pub fn parse_env_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(format!("invalid bool {other:?} (use 0/1/true/false)")),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_env_bool;

    #[test]
    fn parse_env_bool_truthy() {
        for s in ["1", "true", "TRUE", "yes", "on"] {
            assert_eq!(parse_env_bool(s).unwrap(), true, "{s}");
        }
    }

    #[test]
    fn parse_env_bool_falsy() {
        for s in ["0", "false", "no", "off", ""] {
            assert_eq!(parse_env_bool(s).unwrap(), false, "{s}");
        }
    }

    #[test]
    fn parse_env_bool_invalid() {
        assert!(parse_env_bool("maybe").is_err());
    }
}
