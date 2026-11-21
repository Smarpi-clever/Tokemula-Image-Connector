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

#[test]
fn desktop_originals_keep_bytes_unique_names_and_outlive_cache_cleanup() {
    use super::PersistentOutput;
    let directory = tempfile::tempdir().unwrap();
    let desktop = directory.path().join("桌面 空格");
    fs::create_dir(&desktop).unwrap();
    let before = fs::metadata(&desktop).unwrap().permissions();
    let output = PersistentOutput {
        root: desktop.join("Tokemula-Images"),
    };
    output.preflight().unwrap();
    let mut saved = Vec::new();
    for mime in ["image/png", "image/jpeg", "image/webp"] {
        saved.push(output.save(b"complete original\0\xff", mime).unwrap());
    }
    let edited = output
        .save(&fs::read(&saved[0]).unwrap(), "image/png")
        .unwrap();
    assert_ne!(edited, saved[0]);
    let reopened = PersistentOutput {
        root: output.root.clone(),
    };
    assert_ne!(
        reopened.save(b"next original", "image/png").unwrap(),
        saved[0]
    );
    let cache = OutputCache {
        root: directory.path().join("cache"),
    };
    let old = cache.save(b"temporary", "image/png", UNIX_EPOCH).unwrap();
    cache
        .cleanup(UNIX_EPOCH + Duration::from_secs(2 * 86_400))
        .unwrap();
    assert!(!old.path.exists());
    for path in saved.iter().chain([&edited]) {
        assert_eq!(fs::read(path).unwrap(), b"complete original\0\xff");
        assert!(path.is_absolute());
        assert_eq!(
            path.parent(),
            Some(output.root.canonicalize().unwrap().as_path())
        );
        assert!(super::managed_expiry(path).is_none());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            before.mode(),
            fs::metadata(&desktop).unwrap().permissions().mode()
        );
    }
}

#[cfg(unix)]
#[test]
fn persistent_preflight_respects_existing_directory_permissions() {
    use super::PersistentOutput;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Tokemula-Images");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let output = PersistentOutput { root: root.clone() };
    let result = output.preflight();
    let mode = fs::metadata(&root).unwrap().permissions().mode() & 0o777;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(mode, 0o500);
    let error = result.unwrap_err();
    assert_eq!(error.code, "image_storage_unavailable");
    assert_eq!(error.submission, crate::http::Submission::NotSubmitted);
}

#[cfg(unix)]
#[test]
fn desktop_symlink_resolves_without_changing_the_desktop() {
    use super::PersistentOutput;
    let directory = tempfile::tempdir().unwrap();
    let actual = directory.path().join("实际 桌面");
    fs::create_dir(&actual).unwrap();
    let link = directory.path().join("Desktop");
    std::os::unix::fs::symlink(&actual, &link).unwrap();
    let output = PersistentOutput {
        root: link.join("Tokemula-Images"),
    };
    let saved = output.save(b"original", "image/png").unwrap();
    assert_eq!(
        saved.parent().unwrap(),
        actual.canonicalize().unwrap().join("Tokemula-Images")
    );
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
}
