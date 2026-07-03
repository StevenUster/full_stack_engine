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
    #[error("file type is not allowed")]
    InvalidType,
    #[error("failed to create upload directory: {0}")]
    CreateDir(std::io::Error),
    #[error("failed to save uploaded file: {0}")]
    Save(std::io::Error),
}

impl From<UploadError> for crate::error::AppError {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::InvalidSize(_) | UploadError::InvalidType => {
                crate::error::AppError::BadRequest(e.to_string())
            }
            _ => crate::error::AppError::Internal(e.to_string()),
        }
    }
}

/// Saves `temp` under `uploads/<dest_dir>/<prefix>_<uuid>.<ext>`, rejecting
/// empty files and anything over `max_bytes`. The original filename's
/// extension must be one of `allowed_extensions` (case-insensitive); a file
/// with a missing or disallowed extension is rejected rather than silently
/// renamed — untrusted input stays untrusted.
///
/// Returns the web-facing path (e.g. `/uploads/avatars/42_<uuid>.png`), ready
/// to store in the database and serve via the framework's `/uploads` static
/// mount.
///
/// # Errors
///
/// Returns [`UploadError`] if `temp` is empty, too large, or has a disallowed
/// extension, or if the destination directory/file couldn't be created.
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
        .ok_or(UploadError::InvalidType)?;

    let filename = format!("{prefix}_{}.{ext}", uuid::Uuid::new_v4());
    let target = format!("{target_dir}/{filename}");

    std::fs::copy(temp.file.path(), &target).map_err(UploadError::Save)?;

    Ok(format!("/{target_dir}/{filename}"))
}
