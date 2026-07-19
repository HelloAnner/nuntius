use crate::{config::ClientConfig, protocol::*, store::ClientStore};
use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};
use time::{Duration, OffsetDateTime};

const REF_TTL_SECS: i64 = 300;
const PAGE_SIZE: usize = 100;

struct ResolvedDirectory {
    logical: PathBuf,
    canonical: PathBuf,
}

pub async fn roots(
    config: &ClientConfig,
    store: &ClientStore,
    device_id: &str,
) -> Result<DirectoryListResponse> {
    let mut entries = Vec::new();
    for configured in &config.allowed_roots {
        let canonical = match canonical_directory(configured) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(path=%configured.display(),error=?error,"configured directory root is unavailable");
                continue;
            }
        };
        if excluded_pair(configured, &canonical)? {
            continue;
        }
        match entry_for(config, store, configured).await {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                tracing::warn!(path=%configured.display(),error=?error,"configured directory root cannot be browsed");
            }
        }
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
    let parent = resolve_reference(config, store, parent_ref).await?;
    let offset = match cursor {
        Some(value) => value.parse::<usize>().context("invalid directory cursor")?,
        None => 0,
    };
    let mut paths = fs::read_dir(&parent.logical)
        .with_context(|| format!("cannot read {}", parent.logical.display()))?
        .filter_map(|item| item.ok())
        .map(|item| item.path())
        .filter(|path| browsable_directory(config, path))
        .collect::<Vec<_>>();
    paths.sort_by_key(|p| p.file_name().map(|v| v.to_string_lossy().to_lowercase()));
    let total = paths.len();
    let mut entries = Vec::new();
    for path in paths.into_iter().skip(offset).take(PAGE_SIZE) {
        match entry_for(config, store, &path).await {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                tracing::debug!(path=%path.display(),error=?error,"directory entry disappeared while listing");
            }
        }
    }
    let next = (offset + PAGE_SIZE < total).then(|| (offset + PAGE_SIZE).to_string());
    Ok(response(
        device_id,
        parent
            .logical
            .file_name()
            .map(|v| v.to_string_lossy().into_owned()),
        breadcrumb(config, &parent.logical, &parent.canonical),
        entries,
        next,
    ))
}

pub async fn resolve(
    config: &ClientConfig,
    store: &ClientStore,
    directory_ref: &str,
) -> Result<PathBuf> {
    Ok(resolve_reference(config, store, directory_ref)
        .await?
        .canonical)
}

async fn resolve_reference(
    config: &ClientConfig,
    store: &ClientStore,
    directory_ref: &str,
) -> Result<ResolvedDirectory> {
    let raw = store
        .directory_ref_resolve(directory_ref)
        .await?
        .context("directory reference is invalid or expired")?;
    let canonical = canonical_directory(&raw)?;
    ensure_allowed(config, &raw, &canonical)?;
    if excluded_pair(&raw, &canonical)? {
        bail!("directory is excluded")
    };
    Ok(ResolvedDirectory {
        logical: raw,
        canonical,
    })
}

pub fn validate_project_path(config: &ClientConfig, path: &Path) -> Result<PathBuf> {
    let canonical = canonical_directory(path)?;
    ensure_allowed(config, path, &canonical)?;
    if excluded_pair(path, &canonical)? {
        bail!("directory is excluded")
    }
    Ok(canonical)
}

pub fn canonical_project_path(path: &Path) -> Result<PathBuf> {
    canonical_directory(path)
}

async fn entry_for(
    config: &ClientConfig,
    store: &ClientStore,
    path: &Path,
) -> Result<DirectoryEntry> {
    let metadata = fs::symlink_metadata(path)?;
    let symlink = metadata.file_type().is_symlink();
    let canonical = canonical_directory(path)?;
    ensure_allowed(config, path, &canonical)?;
    if excluded_pair(path, &canonical)? {
        bail!("directory is excluded")
    }
    let name = path
        .file_name()
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| canonical.display().to_string());
    let has_children = fs::read_dir(path)
        .ok()
        .map(|mut it| {
            it.any(|value| {
                value
                    .ok()
                    .is_some_and(|entry| browsable_directory(config, &entry.path()))
            })
        })
        .unwrap_or(false);
    let git_kind = canonical.join(".git").exists().then(|| "repository".into());
    let project_id = store.project_by_path(&canonical).await?;
    // Keep the logical path in the short-lived reference. For a symlink this
    // proves the target was reached through a link located under an allowed
    // root; project creation still receives the canonical target below.
    let directory_ref = store.directory_ref_create(path, REF_TTL_SECS).await?;
    Ok(DirectoryEntry {
        name,
        directory_ref,
        breadcrumb: breadcrumb(config, path, &canonical),
        has_children,
        git_kind,
        project_id: project_id.clone(),
        selectable: project_id.is_none(),
        symlink,
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
fn ensure_allowed(config: &ClientConfig, logical: &Path, canonical: &Path) -> Result<()> {
    let allowed = config
        .allowed_roots
        .iter()
        .any(|root| logical.starts_with(root))
        || canonical_roots(config)
            .iter()
            .any(|root| canonical.starts_with(root));
    if allowed {
        Ok(())
    } else {
        bail!("directory is outside allowed_roots")
    }
}
fn breadcrumb(config: &ClientConfig, logical: &Path, canonical: &Path) -> Vec<String> {
    let logical_root = config
        .allowed_roots
        .iter()
        .filter(|root| logical.starts_with(root))
        .max_by_key(|root| root.components().count());
    if let Some(root) = logical_root {
        return breadcrumb_from_root(root, logical);
    }
    let canonical_root = canonical_roots(config)
        .into_iter()
        .filter(|root| canonical.starts_with(root))
        .max_by_key(|root| root.components().count());
    canonical_root
        .map(|root| breadcrumb_from_root(&root, canonical))
        .unwrap_or_default()
}
fn breadcrumb_from_root(root: &Path, path: &Path) -> Vec<String> {
    std::iter::once(
        root.file_name()
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
    )
    .chain(path.strip_prefix(root).ok().into_iter().flat_map(|p| {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
    }))
    .collect()
}
fn browsable_directory(config: &ClientConfig, path: &Path) -> bool {
    let Ok(canonical) = canonical_directory(path) else {
        return false;
    };
    ensure_allowed(config, path, &canonical).is_ok()
        && !excluded_pair(path, &canonical).unwrap_or(true)
}
fn excluded_pair(logical: &Path, canonical: &Path) -> Result<bool> {
    Ok(excluded(logical)? || excluded(canonical)?)
}
fn excluded(path: &Path) -> Result<bool> {
    let nuntius = crate::config::data_dir()?;
    Ok(path == nuntius || path.starts_with(nuntius))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_constant_is_bounded() {
        const { assert!(PAGE_SIZE <= 100) }
    }

    #[tokio::test]
    async fn lists_hidden_and_symlinked_directories_and_rejects_bad_cursors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(root.join("visible")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::create_dir(outside.join("nested")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

        let config = ClientConfig {
            allowed_roots: vec![root.clone()],
            ..ClientConfig::default()
        };
        let store = ClientStore::open(temp.path()).await.unwrap();
        let parent_ref = store.directory_ref_create(&root, 300).await.unwrap();
        let page = list(&config, &store, "dev_test", &parent_ref, None)
            .await
            .unwrap();
        let names = page
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        #[cfg(unix)]
        assert_eq!(names, vec![".hidden", "linked", "visible"]);
        #[cfg(not(unix))]
        assert_eq!(names, vec![".hidden", "visible"]);
        #[cfg(unix)]
        {
            let linked = page
                .entries
                .iter()
                .find(|entry| entry.name == "linked")
                .unwrap();
            assert!(linked.symlink);
            assert_eq!(
                resolve(&config, &store, &linked.directory_ref)
                    .await
                    .unwrap(),
                std::fs::canonicalize(outside).unwrap()
            );
            let linked_page = list(&config, &store, "dev_test", &linked.directory_ref, None)
                .await
                .unwrap();
            assert_eq!(linked_page.entries[0].name, "nested");
        }
        assert!(
            list(&config, &store, "dev_test", &parent_ref, Some("bad"))
                .await
                .is_err()
        );
    }
}
