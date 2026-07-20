#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::config;
use anyhow::{Context, Result, bail};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const LABEL: &str = "com.helloanner.nuntius-client";
const AGENT_HOST_LABEL: &str = "com.helloanner.nuntius-agent-host";
const LAUNCHCTL: &str = "/bin/launchctl";

pub fn start() -> Result<()> {
    ensure_agent_host()?;
    let executable = std::env::current_exe().context("resolve nuntius-client executable")?;
    let plist = launch_agent_path()?;
    let changed = install_launch_agent(&plist, &executable)?;
    let domain = launch_domain();
    let target = launch_target();
    let loaded = is_loaded()?;

    if loaded && changed {
        checked_launchctl([OsStr::new("bootout"), OsStr::new(target.as_str())])?;
    }

    if !loaded || changed {
        checked_launchctl([
            OsStr::new("bootstrap"),
            OsStr::new(domain.as_str()),
            plist.as_os_str(),
        ])?;
    } else {
        checked_launchctl([
            OsStr::new("kickstart"),
            OsStr::new("-k"),
            OsStr::new(target.as_str()),
        ])?;
    }

    println!("started nuntius-client under macOS launchd");
    println!("service: {LABEL}");
    println!("log: {}", config::log_path()?.display());
    Ok(())
}

pub fn ensure_agent_host() -> Result<()> {
    let executable = std::env::current_exe().context("resolve nuntius-client executable")?;
    let plist = agent_host_launch_agent_path()?;
    let contents = launch_agent_plist(
        AGENT_HOST_LABEL,
        &executable,
        "agent-host",
        &config::data_dir()?,
        &config::log_path()?,
        &std::env::var("PATH").unwrap_or_else(|_| {
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()
        }),
    );
    install_launch_agent_contents(&plist, &contents)?;
    if !is_label_loaded(AGENT_HOST_LABEL)? {
        let domain = launch_domain();
        checked_launchctl([
            OsStr::new("bootstrap"),
            OsStr::new(domain.as_str()),
            plist.as_os_str(),
        ])?;
    }
    Ok(())
}

/// Stops launchd ownership and removes the plist so an explicit `stop` remains
/// stopped across the next login. `start` recreates it atomically.
pub fn stop() -> Result<bool> {
    let plist = launch_agent_path()?;
    let loaded = is_loaded()?;
    if loaded {
        let target = launch_target();
        checked_launchctl([OsStr::new("bootout"), OsStr::new(target.as_str())])?;
    }
    match fs::remove_file(&plist) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("remove {}", plist.display()));
        }
    }
    Ok(loaded)
}

pub fn is_loaded() -> Result<bool> {
    is_label_loaded(LABEL)
}

fn is_label_loaded(label: &str) -> Result<bool> {
    let target = launch_target_for(label);
    let output = launchctl([OsStr::new("print"), OsStr::new(target.as_str())])?;
    Ok(output.status.success())
}

fn install_launch_agent(path: &Path, executable: &Path) -> Result<bool> {
    let contents = launch_agent_plist(
        LABEL,
        executable,
        "run",
        &config::data_dir()?,
        &config::log_path()?,
        &std::env::var("PATH").unwrap_or_else(|_| {
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()
        }),
    );
    install_launch_agent_contents(path, &contents)
}

fn install_launch_agent_contents(path: &Path, contents: &str) -> Result<bool> {
    if fs::read(path).ok().as_deref() == Some(contents.as_bytes()) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .context("LaunchAgent path has no parent directory")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("plist.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("create {}", temporary.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("install LaunchAgent {}", path.display()))?;
    Ok(true)
}

fn launch_agent_plist(
    label: &str,
    executable: &Path,
    subcommand: &str,
    data_dir: &Path,
    log_path: &Path,
    path: &str,
) -> String {
    let home = data_dir.parent().unwrap_or(data_dir);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>{subcommand}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>ExitTimeOut</key>
  <integer>15</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>WorkingDirectory</key>
  <string>{data_dir}</string>
  <key>StandardOutPath</key>
  <string>{log_path}</string>
  <key>StandardErrorPath</key>
  <string>{log_path}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key>
    <string>{home}</string>
    <key>PATH</key>
    <string>{path}</string>
    <key>NUNTIUS_LAUNCHD_MANAGED</key>
    <string>1</string>
  </dict>
  <key>Umask</key>
  <integer>63</integer>
</dict>
</plist>
"#,
        label = label,
        subcommand = subcommand,
        executable = xml_escape(&executable.to_string_lossy()),
        data_dir = xml_escape(&data_dir.to_string_lossy()),
        log_path = xml_escape(&log_path.to_string_lossy()),
        home = xml_escape(&home.to_string_lossy()),
        path = xml_escape(path),
    )
}

fn launch_agent_path() -> Result<PathBuf> {
    let home = config::data_dir()?
        .parent()
        .context("client data directory has no home directory")?
        .to_path_buf();
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn agent_host_launch_agent_path() -> Result<PathBuf> {
    let home = config::data_dir()?
        .parent()
        .context("client data directory has no home directory")?
        .to_path_buf();
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{AGENT_HOST_LABEL}.plist")))
}

fn launch_domain() -> String {
    // SAFETY: geteuid has no preconditions and does not dereference memory.
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

fn launch_target() -> String {
    launch_target_for(LABEL)
}

fn launch_target_for(label: &str) -> String {
    format!("{}/{label}", launch_domain())
}

fn checked_launchctl<'a>(arguments: impl IntoIterator<Item = &'a OsStr>) -> Result<Output> {
    let output = launchctl(arguments)?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!("launchctl failed: {}", error.trim())
    }
    Ok(output)
}

fn launchctl<'a>(arguments: impl IntoIterator<Item = &'a OsStr>) -> Result<Output> {
    Command::new(LAUNCHCTL)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("run macOS launchctl")
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_escapes_paths_and_keeps_expected_supervision() {
        let plist = launch_agent_plist(
            LABEL,
            Path::new("/Applications/Nuntius & Tools/nuntius-client"),
            "run",
            Path::new("/Users/test/.nuntius"),
            Path::new("/Users/test/.nuntius/logs/client.log"),
            "/usr/bin:/A&B/bin",
        );

        assert!(plist.contains("Nuntius &amp; Tools"));
        assert!(plist.contains("/usr/bin:/A&amp;B/bin"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<string>run</string>"));
    }
}
