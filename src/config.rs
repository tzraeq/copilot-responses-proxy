use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const APP_NAME: &str = "copilot-responses-proxy";
pub const DEFAULT_UPSTREAM_URL: &str = "https://api.freshid.top/v1/responses";
pub const REASONING_EFFORTS: [ReasoningEffort; 5] = [
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub listen_host: String,
    pub listen_port: u16,
    pub upstream_url: String,
    pub drop_truncation: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub active_token: Option<String>,
    pub tokens: Vec<TokenProfile>,
    pub log_requests: bool,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenProfile {
    pub id: String,
    pub label: String,
    pub value: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 8787,
            upstream_url: DEFAULT_UPSTREAM_URL.to_string(),
            drop_truncation: true,
            reasoning_effort: None,
            active_token: None,
            tokens: Vec::new(),
            log_requests: true,
        }
    }
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }
}

impl fmt::Display for ReasoningEffort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ReasoningEffort {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            _ => bail!(
                "invalid reasoning effort `{value}`; expected one of: minimal, low, medium, high, xhigh"
            ),
        }
    }
}

impl AppConfig {
    pub fn endpoint(&self) -> String {
        format!(
            "http://{}:{}/v1/responses",
            self.listen_host, self.listen_port
        )
    }

    pub fn active_token_profile(&self) -> Option<&TokenProfile> {
        let active_id = self.active_token.as_ref()?;
        self.tokens.iter().find(|token| &token.id == active_id)
    }

    pub fn active_token_value(&self) -> Option<&str> {
        self.active_token_profile()
            .map(|token| token.value.trim())
            .filter(|value| !value.is_empty())
    }

    pub fn upsert_token(&mut self, id: String, label: Option<String>, value: String) -> Result<()> {
        validate_token_id(&id)?;
        if value.trim().is_empty() {
            bail!("token value cannot be empty");
        }

        let label = label.unwrap_or_else(|| id.clone());
        if let Some(existing) = self.tokens.iter_mut().find(|token| token.id == id) {
            existing.label = label;
            existing.value = value;
        } else {
            self.tokens.push(TokenProfile {
                id: id.clone(),
                label,
                value,
            });
        }

        if self.active_token.is_none() {
            self.active_token = Some(id);
        }

        Ok(())
    }

    pub fn use_token(&mut self, id: &str) -> Result<()> {
        if !self.tokens.iter().any(|token| token.id == id) {
            bail!("token profile `{id}` does not exist");
        }
        self.active_token = Some(id.to_string());
        Ok(())
    }

    pub fn clear_active_token(&mut self) {
        self.active_token = None;
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = Some(effort);
    }

    pub fn clear_reasoning_effort(&mut self) {
        self.reasoning_effort = None;
    }

    pub fn remove_token(&mut self, id: &str) -> Result<()> {
        let before = self.tokens.len();
        self.tokens.retain(|token| token.id != id);
        if self.tokens.len() == before {
            bail!("token profile `{id}` does not exist");
        }

        if self.active_token.as_deref() == Some(id) {
            self.active_token = self.tokens.first().map(|token| token.id.clone());
        }

        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        if self.listen_host.trim().is_empty() {
            bail!("listen_host cannot be empty");
        }
        if self.listen_port == 0 {
            bail!("listen_port cannot be 0");
        }
        reqwest::Url::parse(&self.upstream_url)
            .with_context(|| format!("invalid upstream_url `{}`", self.upstream_url))?;
        for token in &self.tokens {
            validate_token_id(&token.id)?;
            if token.label.trim().is_empty() {
                bail!("token `{}` label cannot be empty", token.id);
            }
        }
        Ok(())
    }
}

pub fn app_config_dir() -> PathBuf {
    if let Ok(path) = env::var("COPILOT_RESPONSES_PROXY_CONFIG_DIR") {
        return PathBuf::from(path);
    }

    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

pub fn app_data_dir() -> PathBuf {
    if let Ok(path) = env::var("COPILOT_RESPONSES_PROXY_DATA_DIR") {
        return PathBuf::from(path);
    }

    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(app_config_dir)
        .join(APP_NAME)
}

pub fn default_config_path() -> PathBuf {
    if let Ok(path) = env::var("COPILOT_RESPONSES_PROXY_CONFIG") {
        return PathBuf::from(path);
    }

    app_config_dir().join("config.json")
}

pub fn default_log_dir() -> PathBuf {
    app_data_dir().join("logs")
}

pub fn load_or_create_config() -> Result<(PathBuf, AppConfig)> {
    let path = default_config_path();
    if !path.exists() {
        let config = AppConfig::default();
        save_config(&path, &config)?;
        return Ok((path, config));
    }

    let config = load_config(&path)?;
    Ok((path, config))
}

pub fn load_config(path: &Path) -> Result<AppConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config `{}`", path.display()))?;
    let config: AppConfig = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse config `{}`", path.display()))?;
    config.validate()?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &AppConfig) -> Result<()> {
    config.validate()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir `{}`", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(config)?;
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("failed to write config `{}`", path.display()))?;
    Ok(())
}

fn validate_token_id(id: &str) -> Result<()> {
    if id.trim().is_empty() {
        bail!("token id cannot be empty");
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("token id `{id}` may only contain ASCII letters, digits, dash, underscore, or dot");
    }
    Ok(())
}
