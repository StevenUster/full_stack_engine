//! Generic multipart file-upload saving. Handles the common "validate size,
//! pick a safe extension, write under `uploads/<dest_dir>/`" dance so apps
//! don't have to re-implement it for every upload field (avatars, letter
//! templates, attachments, ...).

use actix_multipart::form::tempfile::TempFile;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("file is empty or exceeds the {0}-byte limit")]
    InvalidSize(usize),
    #[error("failed to create upload directory: {0}")]
    CreateDir(std::io::Error),
    #[error("failed to save uploaded file: {0}")]
    Save(std::io::Error),
}

impl From<UploadError> for crate::error::AppError {
    fn from(e: UploadError) -> Self {
        crate::error::AppError::Internal(e.to_string())
    }
}

/// Saves `temp` under `uploads/<dest_dir>/<prefix>_<uuid>.<ext>`, rejecting
/// empty files and anything over `max_bytes`. `ext` is taken from the
/// original filename when it's one of `allowed_extensions` (case-insensitive),
/// otherwise the first entry of `allowed_extensions` is used.
///
/// Returns the web-facing path (e.g. `/uploads/avatars/42_<uuid>.png`), ready
/// to store in the database and serve via the framework's `/uploads` static
/// mount.
///
/// # Errors
///
/// Returns [`UploadError`] if `temp` is empty or too large, or if the
/// destination directory/file couldn't be created.
pub fn save_upload(
    temp: &TempFile,
    dest_dir: &str,
    prefix: &str,
    allowed_extensions: &[&str],
    max_bytes: usize,
) -> Result<String, UploadError> {
    if temp.size == 0 || temp.size > max_bytes {
        return Err(UploadError::InvalidSize(max_bytes));
    }

    let target_dir = format!("uploads/{dest_dir}");
    std::fs::create_dir_all(&target_dir).map_err(UploadError::CreateDir)?;

    let ext = temp
        .file_name
        .as_deref()
        .and_then(|n| std::path::Path::new(n).extension())
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .filter(|e| allowed_extensions.contains(&e.as_str()))
        .or_else(|| allowed_extensions.first().map(|e| (*e).to_string()))
        .unwrap_or_else(|| "bin".to_string());

    let filename = format!("{prefix}_{}.{ext}", uuid::Uuid::new_v4());
    let target = format!("{target_dir}/{filename}");

    std::fs::copy(temp.file.path(), &target).map_err(UploadError::Save)?;

    Ok(format!("/{target_dir}/{filename}"))
}
