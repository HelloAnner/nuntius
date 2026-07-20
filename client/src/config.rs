use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use url::Url;

pub const CONFIG_FILE: &str = "config.toml";
pub const DATABASE_FILE: &str = "nuntius-client.db";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub server_url: String,
    pub allow_insecure_http: bool,
    pub local_bind: SocketAddr,
    pub display_name: String,
    pub device_id: Option<String>,
    pub allowed_roots: Vec<PathBuf>,
    pub codex_command: String,
    pub codex_args: Vec<String>,
    pub kimi_command: String,
    pub kimi_args: Vec<String>,
    pub kimi_server_url: String,
    pub log_format: String,
    pub auto_update: bool,
    pub update_interval_seconds: u64,
}

impl Default for ClientConfig {
    fn default() -> Self {
        let home = BaseDirs::new()
            .map(|v| v.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let display_name = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "Nuntius Device".into());
        Self {
            server_url: "http://127.0.0.1:8080".into(),
            allow_insecure_http: false,
            local_bind: "127.0.0.1:7331".parse().expect("static address"),
            display_name,
            device_id: None,
            allowed_roots: vec![home],
            codex_command: "codex".into(),
            codex_args: vec!["app-server".into()],
            kimi_command: "kimi".into(),
            kimi_args: vec![
                "web".into(),
                "--no-open".into(),
                "--port".into(),
                "58627".into(),
            ],
            kimi_server_url: "http://127.0.0.1:58627".into(),
            log_format: "pretty".into(),
            auto_update: true,
            update_interval_seconds: 60,
        }
    }
}

pub fn data_dir() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .context("cannot resolve user home directory")?
        .home_dir()
        .join(".nuntius"))
}
pub fn config_path() -> Result<PathBuf> {
    Ok(data_dir()?.join(CONFIG_FILE))
}

impl ClientConfig {
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        Self::load_from_path(&path)
    }
    fn load_from_path(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path).with_context(|| {
            format!(
                "failed to read {}; run `nuntius-client init`",
                path.display()
            )
        })?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let path = config_path()?;
        atomic_private_write(&path, toml::to_string_pretty(self)?.as_bytes())
    }
    pub fn update_display_name(display_name: &str) -> Result<String> {
        Self::update_display_name_at(&config_path()?, display_name)
    }
    fn update_display_name_at(path: &Path, display_name: &str) -> Result<String> {
        let display_name = normalized_display_name(display_name)?.to_owned();
        let mut config = Self::load_from_path(path)?;
        if config.display_name == display_name {
            return Ok(display_name);
        }
        config.display_name = display_name.clone();
        config.validate()?;
        atomic_private_write(path, toml::to_string_pretty(&config)?.as_bytes())?;
        Ok(display_name)
    }
    pub fn validate(&self) -> Result<()> {
        normalized_display_name(&self.display_name)?;
        let url = Url::parse(&self.server_url).context("server_url is invalid")?;
        match url.scheme() {
            "https" => {}
            "http" => {
                let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
                if !local && !self.allow_insecure_http {
                    bail!("non-loopback HTTP requires allow_insecure_http = true")
                }
            }
            other => bail!("server_url must use http or https, got {other}"),
        }
        if !self.local_bind.ip().is_loopback() {
            bail!(
                "local_bind must be loopback; the local console is intentionally not exposed to the LAN"
            )
        }
        if self.allowed_roots.is_empty() {
            bail!("allowed_roots must contain at least one directory")
        }
        if self.allowed_roots.iter().any(|root| !root.is_absolute()) {
            bail!("every allowed_roots entry must be an absolute path")
        }
        let kimi_url = Url::parse(&self.kimi_server_url).context("kimi_server_url is invalid")?;
        if kimi_url.scheme() != "http"
            || !matches!(kimi_url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        {
            bail!("kimi_server_url must be a loopback HTTP URL")
        }
        if self.kimi_command.trim().is_empty()
            || self
                .kimi_args
                .iter()
                .any(|argument| argument.is_empty() || argument.contains('\0'))
        {
            bail!("kimi_command and kimi_args must not contain empty or NUL arguments")
        }
        if self.auto_update && self.update_interval_seconds < 10 {
            bail!("update_interval_seconds must be at least 10")
        }
        Ok(())
    }
    pub fn transport_security(&self) -> crate::protocol::TransportSecurity {
        if self.server_url.starts_with("https://") {
            crate::protocol::TransportSecurity::Secure
        } else {
            crate::protocol::TransportSecurity::Insecure
        }
    }
}

fn normalized_display_name(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        bail!("display_name must contain 1 to 128 bytes")
    }
    Ok(value)
}

pub fn initialize(force: bool) -> Result<PathBuf> {
    let root = data_dir()?;
    fs::create_dir_all(&root)?;
    private_dir(&root)?;
    for child in ["logs", "run", "secrets", "backups", "attachments"] {
        let path = root.join(child);
        fs::create_dir_all(&path)?;
        private_dir(&path)?;
    }
    let path = root.join(CONFIG_FILE);
    if path.exists() && !force {
        bail!("{} already exists", path.display())
    }
    let config = ClientConfig::default();
    fs::write(&path, toml::to_string_pretty(&config)?)?;
    private_file(&path)?;
    let key_path = root.join("secrets/device-key");
    if !key_path.exists() || force {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        use base64::Engine;
        fs::write(
            &key_path,
            base64::engine::general_purpose::STANDARD.encode(bytes),
        )?;
        private_file(&key_path)?;
    }
    Ok(root)
}

fn atomic_private_write(path: &Path, contents: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let temporary =
        path.with_file_name(format!(".{name}.tmp-{:016x}", rand::rng().random::<u64>()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        private_file(&temporary)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn device_key_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("secrets/device-key"))
}
pub fn pid_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("run/nuntius-client.pid"))
}
pub fn log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("logs/nuntius-client.log"))
}

#[cfg(unix)]
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
#[cfg(not(unix))]
pub(crate) fn private_dir(_path: &Path) -> Result<()> {
    Ok(())
}
#[cfg(unix)]
pub(crate) fn private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
pub(crate) fn private_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_device_display_name_for_server_sync() {
        assert_eq!(
            normalized_display_name("  Studio Mac  ").unwrap(),
            "Studio Mac"
        );
        assert!(normalized_display_name("   ").is_err());
        assert!(normalized_display_name(&"名".repeat(43)).is_err());
    }

    #[test]
    fn display_name_update_preserves_the_rest_of_the_config() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CONFIG_FILE);
        let config = ClientConfig {
            display_name: "Old name".into(),
            codex_command: "custom-codex".into(),
            ..ClientConfig::default()
        };
        fs::write(&path, toml::to_string_pretty(&config).unwrap()).unwrap();

        ClientConfig::update_display_name_at(&path, "  Studio Mac  ").unwrap();

        let updated = ClientConfig::load_from_path(&path).unwrap();
        assert_eq!(updated.display_name, "Studio Mac");
        assert_eq!(updated.codex_command, "custom-codex");
    }
}
