use super::OutputCache;
use std::{
    fs,
    time::{Duration, UNIX_EPOCH},
};

#[test]
fn saved_original_is_complete_private_and_retained_for_24_hours() {
    let directory = tempfile::tempdir().unwrap();
    let cache = OutputCache {
        root: directory.path().join("images"),
    };
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let bytes = b"synthetic original image bytes\0\xff";

    let saved = cache.save(bytes, "image/png", now).unwrap();

    assert_eq!(fs::read(&saved.path).unwrap(), bytes);
    assert_eq!(saved.path.parent(), Some(cache.root.as_path()));
    assert_eq!(saved.expires_at, 1_700_000_000 + 86_400);
    assert_eq!(saved.mime_type, "image/png");
    assert_eq!(saved.path.extension().unwrap(), "png");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&cache.root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&saved.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn reopened_cache_removes_only_expired_owned_files() {
    let directory = tempfile::tempdir().unwrap();
    let cache = OutputCache {
        root: directory.path().join("images"),
    };
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let old = cache.save(b"old image", "image/png", now).unwrap();
    let recent = cache
        .save(b"new image", "image/webp", now + Duration::from_secs(60))
        .unwrap();
    let unrelated = cache.root.join("keep.png");
    let external_input = directory.path().join("user-input.png");
    fs::write(&unrelated, b"unrelated file").unwrap();
    fs::write(&external_input, b"user input").unwrap();
    #[cfg(unix)]
    let linked_input = {
        let path = cache.root.join("tic-image-0-abcdefghijkl.png");
        std::os::unix::fs::symlink(&external_input, &path).unwrap();
        path
    };

    cache
        .cleanup(UNIX_EPOCH + Duration::from_secs(old.expires_at - 1))
        .unwrap();
    assert_eq!(fs::read(&old.path).unwrap(), b"old image");
    let reopened = OutputCache {
        root: cache.root.clone(),
    };
    drop(cache);
    reopened
        .cleanup(UNIX_EPOCH + Duration::from_secs(old.expires_at))
        .unwrap();

    assert!(!old.path.exists());
    assert_eq!(fs::read(&recent.path).unwrap(), b"new image");
    assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated file");
    assert_eq!(fs::read(&external_input).unwrap(), b"user input");
    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(&linked_input)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
