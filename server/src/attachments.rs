use crate::config::{set_private_dir_permissions, set_private_file_permissions};
use anyhow::{Context, Result, bail};
use image::{ImageFormat, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;
const MAX_IMAGE_PIXELS: u64 = 50_000_000;
const THUMBNAIL_EDGE: u32 = 360;

pub struct PreparedImage {
    pub original: Vec<u8>,
    pub thumbnail: Vec<u8>,
    pub original_name: String,
    pub mime_type: &'static str,
    pub extension: &'static str,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

pub async fn prepare(bytes: Vec<u8>, original_name: String) -> Result<PreparedImage> {
    tokio::task::spawn_blocking(move || prepare_blocking(bytes, original_name))
        .await
        .context("image validation task failed")?
}

fn prepare_blocking(bytes: Vec<u8>, original_name: String) -> Result<PreparedImage> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        bail!("image must contain 1 to {MAX_IMAGE_BYTES} bytes");
    }
    let format = image::guess_format(&bytes).context("unsupported or corrupt image")?;
    let (mime_type, extension) = match format {
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::WebP => ("image/webp", "webp"),
        _ => bail!("only JPEG, PNG and WebP images are supported"),
    };
    let (width, height) = ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .context("image dimensions cannot be read")?;
    validate_dimensions(width, height)?;
    let decoded =
        image::load_from_memory_with_format(&bytes, format).context("image cannot be decoded")?;
    let mut thumbnail = Cursor::new(Vec::new());
    decoded
        .thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE)
        .write_to(&mut thumbnail, ImageFormat::WebP)
        .context("cannot encode image thumbnail")?;
    let sha256 = hex::encode(Sha256::digest(&bytes));
    Ok(PreparedImage {
        original: bytes,
        thumbnail: thumbnail.into_inner(),
        original_name: safe_original_name(&original_name),
        mime_type,
        extension,
        sha256,
        width,
        height,
    })
}

fn validate_dimensions(width: u32, height: u32) -> Result<()> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || pixels > MAX_IMAGE_PIXELS {
        bail!("image dimensions exceed the 50 megapixel limit");
    }
    Ok(())
}

pub async fn persist(
    data_dir: &Path,
    user_id: &str,
    attachment_id: &str,
    image: &PreparedImage,
) -> Result<()> {
    let directory = attachment_dir(data_dir, user_id, attachment_id)?;
    for private_directory in [
        data_dir.join("attachments"),
        data_dir.join("attachments").join(user_id),
        directory.clone(),
    ] {
        tokio::fs::create_dir_all(&private_directory).await?;
        set_private_dir_permissions(&private_directory)?;
    }
    atomic_write(
        &directory.join(format!("original.{}", image.extension)),
        &image.original,
    )
    .await?;
    atomic_write(&directory.join("thumbnail.webp"), &image.thumbnail).await?;
    Ok(())
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!(
        "{}.partial",
        path.extension().and_then(|v| v.to_str()).unwrap_or("bin")
    ));
    tokio::fs::write(&temporary, bytes).await?;
    set_private_file_permissions(&temporary)?;
    tokio::fs::rename(&temporary, path).await?;
    set_private_file_permissions(path)?;
    Ok(())
}

pub fn original_path(
    data_dir: &Path,
    user_id: &str,
    attachment_id: &str,
    extension: &str,
) -> Result<PathBuf> {
    Ok(attachment_dir(data_dir, user_id, attachment_id)?.join(format!("original.{extension}")))
}

pub fn thumbnail_path(data_dir: &Path, user_id: &str, attachment_id: &str) -> Result<PathBuf> {
    Ok(attachment_dir(data_dir, user_id, attachment_id)?.join("thumbnail.webp"))
}

pub fn attachment_dir(data_dir: &Path, user_id: &str, attachment_id: &str) -> Result<PathBuf> {
    if !safe_component(user_id) || !safe_component(attachment_id) {
        bail!("invalid attachment path component");
    }
    Ok(data_dir
        .join("attachments")
        .join(user_id)
        .join(attachment_id))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn safe_original_name(value: &str) -> String {
    let name = Path::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let mut cleaned = String::new();
    for character in name.chars().filter(|character| !character.is_control()) {
        if cleaned.len() + character.len_utf8() > 180 {
            break;
        }
        cleaned.push(character);
    }
    if cleaned.trim().is_empty() {
        "image".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_a_thumbnail_and_normalizes_the_name() {
        let mut source = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(32, 18)
            .write_to(&mut source, ImageFormat::Png)
            .unwrap();
        let prepared = prepare_blocking(source.into_inner(), "../截图.png".into()).unwrap();
        assert_eq!(prepared.original_name, "截图.png");
        assert_eq!(prepared.mime_type, "image/png");
        assert_eq!((prepared.width, prepared.height), (32, 18));
        assert_eq!(image::guess_format(&prepared.thumbnail).unwrap(), ImageFormat::WebP);
    }

    #[test]
    fn rejects_unsafe_components() {
        assert!(attachment_dir(Path::new("/tmp/data"), "usr_ok", "../bad").is_err());
        assert!(attachment_dir(Path::new("/tmp/data"), "usr_ok", "att_ok").is_ok());
    }
}
