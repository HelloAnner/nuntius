use crate::{config::ClientConfig, protocol::*, store::ClientStore};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};
use time::{Duration, OffsetDateTime};

const REF_TTL_SECS: i64 = 300;
const PAGE_SIZE: usize = 100;

pub async fn roots(
    config: &ClientConfig,
    store: &ClientStore,
    device_id: &str,
) -> Result<DirectoryListResponse> {
    let mut entries = Vec::new();
    for configured in &config.allowed_roots {
        let path = match canonical_directory(configured) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(path=%configured.display(),error=?error,"configured directory root is unavailable");
                continue;
            }
        };
        if excluded(&path)? {
            continue;
        }
        entries.push(entry_for(config, store, &path).await?);
    }
    Ok(response(device_id, None, Vec::new(), entries, None))
}

pub async fn list(
    config: &ClientConfig,
    store: &ClientStore,
    device_id: &str,
    parent_ref: &str,
    cursor: Option<&str>,
) -> Result<DirectoryListResponse> {
    let parent = resolve(config, store, parent_ref).await?;
    let offset = match cursor {
        Some(value) => value.parse::<usize>().context("invalid directory cursor")?,
        None => 0,
    };
    let mut paths = fs::read_dir(&parent)
        .with_context(|| format!("cannot read {}", parent.display()))?
        .filter_map(|item| item.ok())
        .filter(|item| {
            !hidden_name(&item.file_name())
                && item
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
        .map(|item| item.path())
        .filter(|path| !excluded(path).unwrap_or(true))
        .collect::<Vec<_>>();
    paths.sort_by_key(|p| p.file_name().map(|v| v.to_string_lossy().to_lowercase()));
    let total = paths.len();
    let mut entries = Vec::new();
    for path in paths.into_iter().skip(offset).take(PAGE_SIZE) {
        entries.push(entry_for(config, store, &path).await?);
    }
    let next = (offset + PAGE_SIZE < total).then(|| (offset + PAGE_SIZE).to_string());
    Ok(response(
        device_id,
        parent.file_name().map(|v| v.to_string_lossy().into_owned()),
        breadcrumb(config, &parent),
        entries,
        next,
    ))
}

pub async fn resolve(
    config: &ClientConfig,
    store: &ClientStore,
    directory_ref: &str,
) -> Result<PathBuf> {
    let raw = store
        .directory_ref_resolve(directory_ref)
        .await?
        .context("directory reference is invalid or expired")?;
    let path = canonical_directory(&raw)?;
    ensure_allowed(config, &path)?;
    if excluded(&path)? {
        bail!("directory is excluded")
    };
    Ok(path)
}

pub fn validate_project_path(config: &ClientConfig, path: &Path) -> Result<PathBuf> {
    let path = canonical_directory(path)?;
    ensure_allowed(config, &path)?;
    if excluded(&path)? {
        bail!("directory is excluded")
    }
    Ok(path)
}

async fn entry_for(
    config: &ClientConfig,
    store: &ClientStore,
    path: &Path,
) -> Result<DirectoryEntry> {
    let metadata = fs::symlink_metadata(path)?;
    let symlink = metadata.file_type().is_symlink();
    if symlink {
        bail!("symbolic links cannot be browsed")
    }
    let canonical = canonical_directory(path)?;
    ensure_allowed(config, &canonical)?;
    let name = path
        .file_name()
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| canonical.display().to_string());
    let has_children = fs::read_dir(&canonical)
        .ok()
        .map(|mut it| {
            it.any(|value| {
                value.ok().is_some_and(|entry| {
                    !hidden_name(&entry.file_name())
                        && entry
                            .file_type()
                            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                })
            })
        })
        .unwrap_or(false);
    let git_kind = canonical.join(".git").exists().then(|| "repository".into());
    let project_id = store.project_by_path(&canonical).await?;
    let directory_ref = store.directory_ref_create(&canonical, REF_TTL_SECS).await?;
    Ok(DirectoryEntry {
        name,
        directory_ref,
        breadcrumb: breadcrumb(config, &canonical),
        has_children,
        git_kind,
        project_id: project_id.clone(),
        selectable: project_id.is_none(),
        symlink: false,
    })
}

fn response(
    device_id: &str,
    parent_name: Option<String>,
    breadcrumb: Vec<String>,
    entries: Vec<DirectoryEntry>,
    next_cursor: Option<String>,
) -> DirectoryListResponse {
    DirectoryListResponse {
        device_id: device_id.into(),
        parent_name,
        breadcrumb,
        entries,
        next_cursor,
        expires_at: (OffsetDateTime::now_utc() + Duration::seconds(REF_TTL_SECS))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("RFC3339"),
    }
}
fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical =
        fs::canonicalize(path).with_context(|| format!("cannot resolve {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("{} is not a directory", canonical.display())
    };
    Ok(canonical)
}
fn canonical_roots(config: &ClientConfig) -> Vec<PathBuf> {
    config
        .allowed_roots
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .collect()
}
fn ensure_allowed(config: &ClientConfig, path: &Path) -> Result<()> {
    if canonical_roots(config)
        .iter()
        .any(|root| path.starts_with(root))
    {
        Ok(())
    } else {
        bail!("directory is outside allowed_roots")
    }
}
fn breadcrumb(config: &ClientConfig, path: &Path) -> Vec<String> {
    let root = canonical_roots(config)
        .into_iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.components().count());
    match root {
        Some(root) => std::iter::once(
            root.file_name()
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string()),
        )
        .chain(path.strip_prefix(root).ok().into_iter().flat_map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
        }))
        .collect(),
        None => Vec::new(),
    }
}
fn excluded(path: &Path) -> Result<bool> {
    let nuntius = crate::config::data_dir()?;
    Ok(path == nuntius || path.starts_with(nuntius))
}

fn hidden_name(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_constant_is_bounded() {
        const { assert!(PAGE_SIZE <= 100) }
    }

    #[tokio::test]
    async fn hides_dot_directories_rejects_symlinks_and_bad_cursors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("visible")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("visible"), root.join("linked")).unwrap();

        let config = ClientConfig {
            allowed_roots: vec![root.clone()],
            ..ClientConfig::default()
        };
        let store = ClientStore::open(temp.path()).await.unwrap();
        let parent_ref = store.directory_ref_create(&root, 300).await.unwrap();
        let page = list(&config, &store, "dev_test", &parent_ref, None)
            .await
            .unwrap();
        assert_eq!(
            page.entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["visible"]
        );
        assert!(
            list(&config, &store, "dev_test", &parent_ref, Some("bad"))
                .await
                .is_err()
        );
    }
}
