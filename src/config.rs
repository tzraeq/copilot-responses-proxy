use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const APP_NAME: &str = "copilot-responses-proxy";
pub const CONFIG_VERSION: &str = "0.6.0";
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
    pub version: String,
    pub listen_host: String,
    pub listen_port: u16,
    pub upstream_url: String,
    pub drop_truncation: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub active_provider: Option<String>,
    pub providers: Vec<ProviderProfile>,
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
pub struct ProviderProfile {
    pub id: String,
    pub label: String,
    pub address: String,
    pub token: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION.to_string(),
            listen_host: "127.0.0.1".to_string(),
            listen_port: 8787,
            upstream_url: DEFAULT_UPSTREAM_URL.to_string(),
            drop_truncation: true,
            reasoning_effort: None,
            active_provider: None,
            providers: Vec::new(),
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

    pub fn active_provider_profile(&self) -> Option<&ProviderProfile> {
        let active_id = self.active_provider.as_ref()?;
        self.providers
            .iter()
            .find(|provider| &provider.id == active_id)
    }

    pub fn active_upstream_url(&self) -> &str {
        self.active_provider_profile()
            .map(|provider| provider.address.as_str())
            .unwrap_or(self.upstream_url.as_str())
    }

    pub fn active_provider_token_value(&self) -> Option<&str> {
        self.active_provider_profile()
            .and_then(|provider| provider.token.as_deref())
            .filter(|value| !value.is_empty())
    }

    pub fn upsert_provider(
        &mut self,
        id: String,
        label: Option<String>,
        address: String,
        token: Option<String>,
    ) -> Result<()> {
        validate_profile_id(&id)?;
        let address = normalize_address(address)?;
        let token = normalize_token(token);

        let label = label.unwrap_or_else(|| default_provider_label(&address).unwrap_or(id.clone()));
        if let Some(existing) = self.providers.iter_mut().find(|provider| provider.id == id) {
            existing.label = label;
            existing.address = address;
            existing.token = token;
        } else {
            self.providers.push(ProviderProfile {
                id: id.clone(),
                label,
                address,
                token,
            });
        }

        if self.active_provider.is_none() {
            self.active_provider = Some(id);
        }

        Ok(())
    }

    pub fn use_provider(&mut self, id: &str) -> Result<()> {
        if !self.providers.iter().any(|provider| provider.id == id) {
            bail!("provider profile `{id}` does not exist");
        }
        self.active_provider = Some(id.to_string());
        Ok(())
    }

    pub fn set_reasoning_effort(&mut self, effort: ReasoningEffort) {
        self.reasoning_effort = Some(effort);
    }

    pub fn clear_reasoning_effort(&mut self) {
        self.reasoning_effort = None;
    }

    pub fn remove_provider(&mut self, id: &str) -> Result<()> {
        let before = self.providers.len();
        self.providers.retain(|provider| provider.id != id);
        if self.providers.len() == before {
            bail!("provider profile `{id}` does not exist");
        }

        if self.active_provider.as_deref() == Some(id) {
            self.active_provider = self.providers.first().map(|provider| provider.id.clone());
        }

        Ok(())
    }

    pub fn normalize(&mut self) -> Result<()> {
        self.version = CONFIG_VERSION.to_string();

        if self.providers.is_empty() {
            let address = normalize_address(self.upstream_url.clone())?;
            self.providers.push(ProviderProfile {
                id: "default".to_string(),
                label: default_provider_label(&address).unwrap_or_else(|| "Default".to_string()),
                address,
                token: None,
            });
        }

        if self.active_provider.as_ref().is_none_or(|active_id| {
            !self
                .providers
                .iter()
                .any(|provider| &provider.id == active_id)
        }) {
            self.active_provider = self.providers.first().map(|provider| provider.id.clone());
        }

        if let Some(provider) = self.active_provider_profile() {
            self.upstream_url = provider.address.clone();
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
        for provider in &self.providers {
            validate_profile_id(&provider.id)?;
            if provider.label.trim().is_empty() {
                bail!("provider `{}` label cannot be empty", provider.id);
            }
            reqwest::Url::parse(&provider.address).with_context(|| {
                format!(
                    "invalid provider `{}` address `{}`",
                    provider.id, provider.address
                )
            })?;
        }
        if let Some(active_provider) = &self.active_provider {
            if !self
                .providers
                .iter()
                .any(|provider| &provider.id == active_provider)
            {
                bail!("active provider `{active_provider}` does not exist");
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
        let mut config = AppConfig::default();
        config.normalize()?;
        save_config(&path, &config)?;
        return Ok((path, config));
    }

    let config = load_config(&path)?;
    Ok((path, config))
}

pub fn load_config(path: &Path) -> Result<AppConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config `{}`", path.display()))?;
    let mut config: AppConfig = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse config `{}`", path.display()))?;
    config.normalize()?;
    config.validate()?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &AppConfig) -> Result<()> {
    let mut config = config.clone();
    config.normalize()?;
    config.validate()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config dir `{}`", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(&config)?;
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("failed to write config `{}`", path.display()))?;
    Ok(())
}

fn validate_profile_id(id: &str) -> Result<()> {
    if id.trim().is_empty() {
        bail!("profile id cannot be empty");
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("profile id `{id}` may only contain ASCII letters, digits, dash, underscore, or dot");
    }
    Ok(())
}

fn normalize_address(address: String) -> Result<String> {
    let address = address.trim().to_string();
    if address.is_empty() {
        bail!("provider address cannot be empty");
    }

    let address = if has_url_scheme(&address) {
        address
    } else {
        format!("{}{}", default_scheme_for_address(&address), address)
    };

    let mut url = reqwest::Url::parse(&address)
        .with_context(|| format!("invalid provider address `{address}`"))?;

    if is_root_url(&url) {
        url.set_path("/v1/responses");
    }

    Ok(url.to_string())
}

fn normalize_token(token: Option<String>) -> Option<String> {
    token
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn has_url_scheme(address: &str) -> bool {
    address.contains("://")
}

fn default_scheme_for_address(address: &str) -> &'static str {
    if address_host(address).is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok()
    }) {
        "http://"
    } else {
        "https://"
    }
}

fn address_host(address: &str) -> Option<&str> {
    let authority = address
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(address)
        .rsplit('@')
        .next()
        .unwrap_or(address);

    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().filter(|host| !host.is_empty());
    }

    authority.split(':').next().filter(|host| !host.is_empty())
}

fn is_root_url(url: &reqwest::Url) -> bool {
    matches!(url.path(), "" | "/") && url.query().is_none() && url.fragment().is_none()
}

fn default_provider_label(address: &str) -> Option<String> {
    reqwest::Url::parse(address)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .filter(|host| !host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_sets_current_config_version() {
        let mut config = AppConfig {
            version: String::new(),
            ..AppConfig::default()
        };
        config.normalize().unwrap();

        assert_eq!(config.version, CONFIG_VERSION);
    }

    #[test]
    fn provider_add_completes_bare_domain_and_defaults_label_to_host() {
        let mut config = AppConfig::default();
        config
            .upsert_provider(
                "main".to_string(),
                None,
                "api.example.com".to_string(),
                None,
            )
            .unwrap();

        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "main")
            .unwrap();
        assert_eq!(provider.address, "https://api.example.com/v1/responses");
        assert_eq!(provider.label, "api.example.com");
        assert_eq!(provider.token, None);
    }

    #[test]
    fn provider_add_completes_bare_ip_with_http() {
        let mut config = AppConfig::default();
        config
            .upsert_provider(
                "local".to_string(),
                None,
                "127.0.0.1:3000".to_string(),
                None,
            )
            .unwrap();

        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "local")
            .unwrap();
        assert_eq!(provider.address, "http://127.0.0.1:3000/v1/responses");
        assert_eq!(provider.label, "127.0.0.1");
    }

    #[test]
    fn provider_add_preserves_non_root_paths() {
        let mut config = AppConfig::default();
        config
            .upsert_provider(
                "relay".to_string(),
                None,
                "relay.example.com/custom/responses".to_string(),
                Some("  ".to_string()),
            )
            .unwrap();

        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "relay")
            .unwrap();
        assert_eq!(
            provider.address,
            "https://relay.example.com/custom/responses"
        );
        assert_eq!(provider.label, "relay.example.com");
        assert_eq!(provider.token, None);
    }
}
