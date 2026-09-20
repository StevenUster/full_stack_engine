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
    #[error("file content does not match its extension")]
    ContentMismatch,
    #[error("failed to create upload directory: {0}")]
    CreateDir(std::io::Error),
    #[error("failed to save uploaded file: {0}")]
    Save(std::io::Error),
}

impl From<UploadError> for crate::error::AppError {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::InvalidSize(_)
            | UploadError::InvalidType
            | UploadError::ContentMismatch => crate::error::AppError::BadRequest(e.to_string()),
            _ => crate::error::AppError::Internal(e.to_string()),
        }
    }
}

/// Checks the file's leading bytes against the magic-byte signature for a
/// known image `ext`. Extensions this function doesn't recognise (non-image
/// uploads, e.g. letter template HTML) pass through unchecked, since there is
/// no single content signature to verify them against.
fn image_content_matches_ext(path: &std::path::Path, ext: &str) -> bool {
    use std::io::Read;

    let sig_matches = |buf: &[u8]| match ext {
        "jpg" | "jpeg" => buf.starts_with(&[0xFF, 0xD8, 0xFF]),
        "png" => buf.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        "gif" => buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a"),
        "webp" => buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP",
        _ => true,
    };

    let mut buf = [0u8; 12];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(n) = file.read(&mut buf) else {
        return false;
    };
    sig_matches(&buf[..n])
}

/// Saves `temp` under `uploads/<dest_dir>/<prefix>_<uuid>.<ext>`, rejecting
/// empty files and anything over `max_bytes`. The original filename's
/// extension must be one of `allowed_extensions` (case-insensitive); a file
/// with a missing or disallowed extension is rejected rather than silently
/// renamed — untrusted input stays untrusted. For recognised image
/// extensions (jpg/jpeg/png/gif/webp), the file's leading bytes must also
/// match that format's signature, so a disguised upload (e.g. an HTML/SVG
/// file renamed `.png`) is rejected rather than served back with an image
/// extension.
///
/// Returns the web-facing path (e.g. `/uploads/avatars/42_<uuid>.png`), ready
/// to store in the database and serve via the framework's `/uploads` static
/// mount.
///
/// # Errors
///
/// Returns [`UploadError`] if `temp` is empty, too large, has a disallowed
/// extension, has content that doesn't match a recognised image extension, or
/// if the destination directory/file couldn't be created.
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

    // Validate before touching the filesystem, so a rejected upload leaves no
    // side effects behind.
    let ext = temp
        .file_name
        .as_deref()
        .and_then(|n| std::path::Path::new(n).extension())
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .filter(|e| allowed_extensions.contains(&e.as_str()))
        .ok_or(UploadError::InvalidType)?;

    if !image_content_matches_ext(temp.file.path(), &ext) {
        return Err(UploadError::ContentMismatch);
    }

    let target_dir = format!("uploads/{dest_dir}");
    std::fs::create_dir_all(&target_dir).map_err(UploadError::CreateDir)?;

    let filename = format!("{prefix}_{}.{ext}", uuid::Uuid::new_v4());
    let target = format!("{target_dir}/{filename}");

    std::fs::copy(temp.file.path(), &target).map_err(UploadError::Save)?;

    Ok(format!("/{target_dir}/{filename}"))
}

/// Deletes a file [`save_upload`] created, given the exact path it returned
/// (e.g. `/uploads/avatars/42_<uuid>.png`).
///
/// Returns `true` if a file was removed. A path that does not name a file
/// directly inside the `uploads/` tree is **ignored and reported as `false`**,
/// never followed: the stored path usually arrives from a database column, and
/// a column is only as trustworthy as everything that has ever written to it.
///
/// This exists because `save_upload` without a counterpart means every app
/// writes its own `std::fs::remove_file(format!(...))`, and each copy has to
/// rediscover that `..` and a nested path must be refused. That check belongs
/// in one place.
///
/// Deletion is best effort: a file already gone, or one the process cannot
/// remove, is `false` rather than an error. Callers delete an upload while
/// clearing the column that referenced it, and a failure there must not
/// abandon the row pointing at a file that may or may not exist.
#[must_use]
pub fn delete_upload(stored_path: &str) -> bool {
    delete_upload_in(std::path::Path::new("uploads"), stored_path)
}

/// [`delete_upload`] against an explicit uploads root, so the path rules are
/// testable without moving the process's working directory (which is global
/// state and would race every other test in the binary).
fn delete_upload_in(root: &std::path::Path, stored_path: &str) -> bool {
    let Some(relative) = stored_path.trim().strip_prefix("/uploads/") else {
        return false;
    };
    // `<dir>/<file>` exactly: one level, no traversal, no absolute escape.
    // Anything else is a path this function did not write.
    let mut segments = relative.split('/');
    let (Some(dir), Some(file), None) = (segments.next(), segments.next(), segments.next()) else {
        return false;
    };
    if dir.is_empty() || file.is_empty() || matches!(dir, "." | "..") || matches!(file, "." | "..")
    {
        return false;
    }
    std::fs::remove_file(root.join(dir).join(file)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_upload(file_name: Option<&str>, size: usize) -> TempFile {
        TempFile {
            file: tempfile::NamedTempFile::new().unwrap(),
            content_type: None,
            file_name: file_name.map(String::from),
            size,
        }
    }

    fn temp_upload_with_bytes(file_name: &str, bytes: &[u8]) -> TempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        TempFile {
            file,
            content_type: None,
            file_name: Some(file_name.to_string()),
            size: bytes.len(),
        }
    }

    const PNG_SIGNATURE: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];

    #[test]
    fn delete_upload_refuses_anything_it_did_not_write() {
        // Traversal, in the two places a path has segments.
        assert!(!delete_upload("/uploads/../../etc/passwd"));
        assert!(!delete_upload("/uploads/avatars/../../../etc/passwd"));
        // Deeper than save_upload ever writes.
        assert!(!delete_upload("/uploads/a/b/c.png"));
        // Not under /uploads at all.
        assert!(!delete_upload("/etc/passwd"));
        assert!(!delete_upload("uploads/avatars/a.png"));
        assert!(!delete_upload("/uploadsx/avatars/a.png"));
        // Degenerate shapes.
        assert!(!delete_upload(""));
        assert!(!delete_upload("/uploads/"));
        assert!(!delete_upload("/uploads/avatars"));
        assert!(!delete_upload("/uploads//a.png"));
        assert!(!delete_upload("/uploads/./a.png"));
    }

    #[test]
    fn delete_upload_removes_the_file_it_is_given() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("avatars")).unwrap();
        let file = root.path().join("avatars").join("7_abc.png");
        std::fs::write(&file, PNG_SIGNATURE).unwrap();

        assert!(delete_upload_in(root.path(), "/uploads/avatars/7_abc.png"));
        assert!(!file.exists());
        // Already gone: reported as `false`, not an error.
        assert!(!delete_upload_in(root.path(), "/uploads/avatars/7_abc.png"));
    }

    #[test]
    fn a_refused_path_is_never_followed() {
        // The point of the path rules: prove nothing outside the root is
        // touched, not merely that the call returns false.
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("secret.txt");
        std::fs::write(&outside, b"keep me").unwrap();
        std::fs::create_dir_all(root.path().join("uploads")).unwrap();

        assert!(!delete_upload_in(
            &root.path().join("uploads"),
            "/uploads/../secret.txt"
        ));
        assert!(
            outside.exists(),
            "traversal must not reach outside the root"
        );
    }

    #[test]
    fn rejects_empty_and_oversized_files() {
        let empty = temp_upload(Some("a.png"), 0);
        assert!(matches!(
            save_upload(&empty, "t", "p", &["png"], 100),
            Err(UploadError::InvalidSize(100))
        ));

        let oversized = temp_upload(Some("a.png"), 101);
        assert!(matches!(
            save_upload(&oversized, "t", "p", &["png"], 100),
            Err(UploadError::InvalidSize(100))
        ));
    }

    #[test]
    fn rejects_missing_and_disallowed_extensions() {
        // Disallowed extension: rejected, never silently renamed.
        let exe = temp_upload(Some("evil.exe"), 10);
        assert!(matches!(
            save_upload(&exe, "t", "p", &["png", "jpg"], 100),
            Err(UploadError::InvalidType)
        ));

        // No filename / no extension at all.
        let nameless = temp_upload(None, 10);
        assert!(matches!(
            save_upload(&nameless, "t", "p", &["png"], 100),
            Err(UploadError::InvalidType)
        ));
        let extensionless = temp_upload(Some("photo"), 10);
        assert!(matches!(
            save_upload(&extensionless, "t", "p", &["png"], 100),
            Err(UploadError::InvalidType)
        ));
    }

    #[test]
    fn size_and_type_errors_map_to_bad_request() {
        use crate::error::AppError;
        assert!(matches!(
            AppError::from(UploadError::InvalidType),
            AppError::BadRequest(_)
        ));
        assert!(matches!(
            AppError::from(UploadError::InvalidSize(1)),
            AppError::BadRequest(_)
        ));
        assert!(matches!(
            AppError::from(UploadError::ContentMismatch),
            AppError::BadRequest(_)
        ));
    }

    #[test]
    fn rejects_content_that_does_not_match_the_claimed_extension() {
        // Plain text disguised as a `.png` — extension is allowed, but the
        // bytes don't carry the PNG signature.
        let fake = temp_upload_with_bytes("a.png", b"<script>alert(1)</script>");
        assert!(matches!(
            save_upload(&fake, "t", "p", &["png"], 100),
            Err(UploadError::ContentMismatch)
        ));
    }

    #[test]
    fn accepts_content_matching_the_claimed_extension() {
        let real_png = temp_upload_with_bytes("a.png", PNG_SIGNATURE);
        let saved = save_upload(&real_png, "t-uploads-test", "p", &["png"], 100).unwrap();
        assert!(saved.starts_with("/uploads/t-uploads-test/p_"));
        assert_eq!(
            std::path::Path::new(&saved).extension(),
            Some("png".as_ref())
        );
        let _ = std::fs::remove_dir_all("uploads/t-uploads-test");
    }

    #[test]
    fn passes_through_non_image_extensions_unchecked() {
        // Non-image extensions (e.g. uploaded HTML templates) have no known
        // signature to check, so content is accepted as-is once the
        // extension allow-list and size limit are satisfied.
        let html = temp_upload_with_bytes("template.html", b"<html>not an image</html>");
        let saved = save_upload(&html, "t-uploads-test2", "p", &["html"], 100).unwrap();
        assert_eq!(
            std::path::Path::new(&saved).extension(),
            Some("html".as_ref())
        );
        let _ = std::fs::remove_dir_all("uploads/t-uploads-test2");
    }
}
