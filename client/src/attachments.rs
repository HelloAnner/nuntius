use crate::{config, pairing, protocol::AttachmentRef};
use anyhow::{Context, Result, bail};
use image::{ImageFormat, ImageReader};
use reqwest::{Client, redirect};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;
const THUMBNAIL_EDGE: u32 = 360;

pub async fn ensure_local(
    config_value: &crate::config::ClientConfig,
    thread_id: &str,
    attachment: &AttachmentRef,
    access_token: &str,
) -> Result<PathBuf> {
    validate_reference(attachment)?;
    let root = config::data_dir()?;
    let directory = attachment_dir(&root, thread_id, &attachment.id)?;
    let destination = directory.join(format!("original.{}", attachment.extension));
    if let Ok(existing) = tokio::fs::read(&destination).await
        && hex::encode(Sha256::digest(&existing)) == attachment.sha256
    {
        return Ok(destination);
    }

    let url = pairing::endpoint(
        config_value,
        &format!("api/v1/device-attachments/{}/content", attachment.id),
    )?;
    let response = Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .redirect(redirect::Policy::none())
        .build()?
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("attachment download failed with {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_BYTES as u64)
    {
        bail!("attachment exceeds the local size limit");
    }
    let bytes = response.bytes().await?.to_vec();
    let expected_size = usize::try_from(attachment.byte_size).context("invalid attachment size")?;
    if bytes.len() != expected_size || bytes.len() > MAX_IMAGE_BYTES {
        bail!("attachment size mismatch");
    }
    let digest = hex::encode(Sha256::digest(&bytes));
    if digest != attachment.sha256 {
        bail!("attachment checksum mismatch");
    }
    let (bytes, detected_mime, detected_extension, width, height, thumbnail) =
        tokio::task::spawn_blocking(move || inspect_and_thumbnail(bytes)).await??;
    if detected_mime != attachment.mime_type || detected_extension != attachment.extension {
        bail!("attachment media type mismatch");
    }
    if width != attachment.width || height != attachment.height {
        bail!("attachment dimensions mismatch");
    }

    for private_directory in [
        root.join("attachments"),
        root.join("attachments").join(thread_id),
        directory.clone(),
    ] {
        tokio::fs::create_dir_all(&private_directory).await?;
        config::private_dir(&private_directory)?;
    }
    atomic_write(&destination, &bytes).await?;
    atomic_write(&directory.join("thumbnail.webp"), &thumbnail).await?;
    Ok(destination)
}

fn inspect_and_thumbnail(
    bytes: Vec<u8>,
) -> Result<(Vec<u8>, &'static str, &'static str, u32, u32, Vec<u8>)> {
    let format = image::guess_format(&bytes).context("unsupported attachment image")?;
    let (mime_type, extension) = match format {
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::WebP => ("image/webp", "webp"),
        _ => bail!("unsupported attachment image format"),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .context("attachment dimensions cannot be read")?;
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        bail!("attachment dimensions are invalid");
    }
    let image = image::load_from_memory_with_format(&bytes, format)?;
    let mut thumbnail = Cursor::new(Vec::new());
    image
        .thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE)
        .write_to(&mut thumbnail, ImageFormat::WebP)?;
    Ok((
        bytes,
        mime_type,
        extension,
        width,
        height,
        thumbnail.into_inner(),
    ))
}

fn validate_reference(attachment: &AttachmentRef) -> Result<()> {
    if attachment.id.is_empty()
        || attachment.id.len() > 128
        || !attachment
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        bail!("invalid attachment id");
    }
    if attachment.sha256.len() != 64
        || !attachment
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("invalid attachment checksum");
    }
    if !matches!(
        (attachment.mime_type.as_str(), attachment.extension.as_str()),
        ("image/jpeg", "jpg") | ("image/png", "png") | ("image/webp", "webp")
    ) {
        bail!("invalid attachment media type");
    }
    let pixels = u64::from(attachment.width) * u64::from(attachment.height);
    if attachment.byte_size <= 0
        || attachment.byte_size > MAX_IMAGE_BYTES as i64
        || pixels == 0
        || pixels > MAX_IMAGE_PIXELS
    {
        bail!("invalid attachment metadata");
    }
    Ok(())
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!(
        "{}.partial",
        path.extension().and_then(|v| v.to_str()).unwrap_or("bin")
    ));
    tokio::fs::write(&temporary, bytes).await?;
    config::private_file(&temporary)?;
    tokio::fs::rename(&temporary, path).await?;
    config::private_file(path)?;
    Ok(())
}

pub fn original_path(
    root: &Path,
    thread_id: &str,
    attachment_id: &str,
    extension: &str,
) -> Result<PathBuf> {
    Ok(attachment_dir(root, thread_id, attachment_id)?.join(format!("original.{extension}")))
}

pub fn thumbnail_path(root: &Path, thread_id: &str, attachment_id: &str) -> Result<PathBuf> {
    Ok(attachment_dir(root, thread_id, attachment_id)?.join("thumbnail.webp"))
}

fn attachment_dir(root: &Path, thread_id: &str, attachment_id: &str) -> Result<PathBuf> {
    if !safe_component(thread_id) || !safe_component(attachment_id) {
        bail!("invalid attachment path");
    }
    Ok(root.join("attachments").join(thread_id).join(attachment_id))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}
