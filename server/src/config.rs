use anyhow::{Context, Result, bail};
use fs2::FileExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    net::SocketAddr,
    path::{Path, PathBuf},
};
use url::Url;

pub const CONFIG_FILE: &str = "config.toml";
pub const DATABASE_FILE: &str = "nuntius-server.db";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub public_base_url: String,
    pub allow_insecure_http: bool,
    pub session_ttl_hours: i64,
    pub device_token_ttl_minutes: i64,
    pub pairing_code_ttl_minutes: i64,
    pub event_retention_hours: i64,
    pub log_format: String,
    pub auto_update: bool,
    pub direct_github_update: bool,
    pub update_interval_seconds: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8080".parse().expect("static socket address"),
            public_base_url: "http://127.0.0.1:8080".into(),
            allow_insecure_http: false,
            session_ttl_hours: 168,
            device_token_ttl_minutes: 15,
            pairing_code_ttl_minutes: 10,
            event_retention_hours: 24,
            log_format: "pretty".into(),
            auto_update: true,
            direct_github_update: true,
            update_interval_seconds: 60,
        }
    }
}

impl ServerConfig {
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(CONFIG_FILE);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let url = Url::parse(&self.public_base_url).context("public_base_url is invalid")?;
        match url.scheme() {
            "https" => {}
            "http" => {
                let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
                if !local && !self.allow_insecure_http {
                    bail!("non-loopback HTTP requires allow_insecure_http = true");
                }
            }
            scheme => bail!("public_base_url scheme must be http or https, got {scheme}"),
        }
        if !self.bind.ip().is_loopback() && !self.allow_insecure_http {
            bail!(
                "non-loopback bind requires allow_insecure_http = true because the Rust listener itself is plain HTTP"
            );
        }
        if self.session_ttl_hours <= 0
            || self.device_token_ttl_minutes <= 0
            || self.pairing_code_ttl_minutes <= 0
            || self.event_retention_hours <= 0
        {
            bail!("token TTL values must be positive");
        }
        if self.auto_update && self.update_interval_seconds < 60 {
            bail!("update_interval_seconds must be at least 60");
        }
        Ok(())
    }

    pub fn is_secure(&self) -> bool {
        self.public_base_url.starts_with("https://")
    }
}

pub fn initialize_data_dir(data_dir: &Path, force: bool) -> Result<InitResult> {
    fs::create_dir_all(data_dir)
        .with_context(|| format!("failed to create {}", data_dir.display()))?;
    set_private_dir_permissions(data_dir)?;
    for child in ["logs", "run", "backups", "secrets"] {
        let path = data_dir.join(child);
        fs::create_dir_all(&path)?;
        set_private_dir_permissions(&path)?;
    }

    let config_path = data_dir.join(CONFIG_FILE);
    if config_path.exists() && !force {
        bail!(
            "{} already exists; use --force only if you intend to replace it",
            config_path.display()
        );
    }
    let config = ServerConfig::default();
    fs::write(&config_path, toml::to_string_pretty(&config)?)?;
    set_private_file_permissions(&config_path)?;

    let bootstrap_token = random_secret(32);
    let bootstrap_path = data_dir.join("secrets/bootstrap-token");
    fs::write(&bootstrap_path, format!("{bootstrap_token}\n"))?;
    set_private_file_permissions(&bootstrap_path)?;

    let server_secret_path = data_dir.join("secrets/server-secret");
    fs::write(&server_secret_path, format!("{}\n", random_secret(64)))?;
    set_private_file_permissions(&server_secret_path)?;

    Ok(InitResult {
        data_dir: data_dir.to_path_buf(),
        bootstrap_token,
    })
}

pub struct InitResult {
    pub data_dir: PathBuf,
    pub bootstrap_token: String,
}

pub struct DataDirLock {
    _file: File,
}

impl DataDirLock {
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("run/server.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.try_lock_exclusive().with_context(|| {
            format!(
                "another nuntius-server is already using {}",
                data_dir.display()
            )
        })?;
        set_private_file_permissions(&path)?;
        Ok(Self { _file: file })
    }
}

pub fn random_secret(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
