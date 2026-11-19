use std::env;
use std::error::Error;
use std::fmt;

pub const DEFAULT_DATA_PATH: &str = "data/bin-list-data.csv";
pub const DEFAULT_HOST: &str = "0.0.0.0";
pub const DEFAULT_PORT: u16 = 8080;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub data_path: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigError {
    EmptyHost,
    EmptyDataPath,
    InvalidPort(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHost => write!(f, "HOST environment variable must not be empty"),
            Self::EmptyDataPath => {
                write!(f, "BIN_DATA_PATH environment variable must not be empty")
            }
            Self::InvalidPort(raw) => write!(f, "invalid PORT environment variable: {raw}"),
        }
    }
}

impl Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    pub fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let data_path = match lookup("BIN_DATA_PATH") {
            Some(val) => {
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::EmptyDataPath);
                }
                trimmed.to_string()
            }
            None => DEFAULT_DATA_PATH.to_string(),
        };

        let host = match lookup("HOST") {
            Some(val) => {
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    return Err(ConfigError::EmptyHost);
                }
                trimmed.to_string()
            }
            None => DEFAULT_HOST.to_string(),
        };

        let port = match lookup("PORT") {
            Some(raw) => raw
                .trim()
                .parse::<u16>()
                .map_err(|_| ConfigError::InvalidPort(raw))?,
            None => DEFAULT_PORT,
        };

        Ok(Self {
            data_path,
            host,
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn map_lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |key| map.get(key).map(|v| (*v).to_string())
    }

    #[test]
    fn uses_defaults_when_env_vars_are_absent() {
        let cfg = Config::from_lookup(|_| None).expect("defaults should be valid");
        assert_eq!(
            cfg,
            Config {
                data_path: DEFAULT_DATA_PATH.to_string(),
                host: DEFAULT_HOST.to_string(),
                port: DEFAULT_PORT,
            }
        );
    }

    #[test]
    fn parses_custom_host_port_and_path() {
        let cfg = Config::from_lookup(map_lookup(&[
            ("HOST", "localhost"),
            ("PORT", "3000"),
            ("BIN_DATA_PATH", "/tmp/bins.csv"),
        ]))
        .expect("custom config should parse");
        assert_eq!(cfg.host, "localhost");
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.data_path, "/tmp/bins.csv");
    }

    #[test]
    fn rejects_invalid_values() {
        assert_eq!(
            Config::from_lookup(map_lookup(&[("PORT", "not-a-number")])),
            Err(ConfigError::InvalidPort("not-a-number".to_string()))
        );
        assert_eq!(
            Config::from_lookup(map_lookup(&[("HOST", "   ")])),
            Err(ConfigError::EmptyHost)
        );
        assert_eq!(
            Config::from_lookup(map_lookup(&[("BIN_DATA_PATH", "")])),
            Err(ConfigError::EmptyDataPath)
        );
    }
}
