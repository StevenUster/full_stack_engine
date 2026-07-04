use actix_multipart::form::tempfile::TempFile;
use full_stack_engine::uploads::save_upload;
use std::io::Write;

/// The success path writes to `uploads/` under the current directory, so this
/// test lives in its own integration-test binary (= its own process) where
/// changing the working directory can't race with other tests.
#[test]
fn saves_valid_upload_under_uploads_dir() {
    let workdir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(workdir.path()).unwrap();

    // Real PNG signature bytes followed by arbitrary payload: content must
    // match the claimed extension, not just carry a plausible size/name.
    let png_bytes: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(png_bytes).unwrap();
    let temp = TempFile {
        file,
        content_type: None,
        file_name: Some("Photo.PNG".to_string()),
        size: png_bytes.len(),
    };

    let web_path = save_upload(&temp, "avatars", "42", &["png", "jpg"], 1024).unwrap();

    // Extension is matched case-insensitively and stored lowercased.
    assert!(web_path.starts_with("/uploads/avatars/42_"));
    assert!(web_path.ends_with(".png"));

    // The returned web path maps 1:1 onto the file on disk.
    let disk_path = web_path.trim_start_matches('/');
    let contents = std::fs::read(disk_path).unwrap();
    assert_eq!(contents, png_bytes);
}
