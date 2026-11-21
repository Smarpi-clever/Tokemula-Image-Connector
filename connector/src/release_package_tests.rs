use super::*;
use ed25519_dalek::{Signer, SigningKey};
use flate2::{Compression, write::GzEncoder};
use std::io::Cursor;

struct Fixture {
    temp: tempfile::TempDir,
    payload: PathBuf,
    trust: PathBuf,
    signer: SigningKey,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let payload = temp.path().join("payload");
        fs::create_dir(&payload).unwrap();
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        getrandom::fill(&mut *seed).unwrap();
        let signer = SigningKey::from_bytes(&seed);
        drop(seed);
        let trust = temp.path().join("trust.txt");
        fs::write(&trust, STANDARD.encode(signer.verifying_key().as_bytes())).unwrap();
        let fixture = Self {
            temp,
            payload,
            trust,
            signer,
        };
        for name in [
            "tokemula-image",
            "tokemula-image-mcp",
            "LICENSE",
            "QUICKSTART.md",
            "THIRD-PARTY/NOTICE.md",
            "THIRD-PARTY/MIT.txt",
        ] {
            let file = fixture.payload.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, format!("synthetic release file {name}")).unwrap();
        }
        fixture.resign(TARGET);
        fixture
    }
    fn resign(&self, target: &str) {
        let mut paths = HashSet::new();
        directory_files(&self.payload, &self.payload, &mut paths).unwrap();
        let mut files: Vec<_> = paths
            .into_iter()
            .filter(|name| !matches!(name.as_str(), "release.json" | "release.sig"))
            .map(|path| ReleaseFile {
                sha256: sha256_file(&self.payload.join(&path)).unwrap(),
                size: fs::metadata(self.payload.join(&path)).unwrap().len(),
                path,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        self.sign_manifest(&ReleaseManifest {
            schema_version: 1,
            product: PRODUCT.into(),
            version: "0.1.0-d2a.1".into(),
            target: target.into(),
            files,
        });
    }
    fn sign_manifest(&self, manifest: &ReleaseManifest) {
        let bytes = serde_json::to_vec(manifest).unwrap();
        fs::write(self.payload.join("release.json"), &bytes).unwrap();
        fs::write(
            self.payload.join("release.sig"),
            STANDARD.encode(self.signer.sign(&bytes).to_bytes()),
        )
        .unwrap();
    }
    fn update(&self, archive: &Path) -> ReleaseUpdate {
        ReleaseUpdate {
            schema_version: 1,
            version: "0.1.0-d2a.1".into(),
            target: TARGET.into(),
            archive_url: "http://127.0.0.1:5000/package.tar.gz".into(),
            archive_sha256: sha256_file(archive).unwrap(),
            archive_size: fs::metadata(archive).unwrap().len(),
        }
    }
    fn normal_archive(&self) -> PathBuf {
        let archive = self.temp.path().join("release.tar.gz");
        let encoder = GzEncoder::new(File::create(&archive).unwrap(), Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let manifest: ReleaseManifest =
            serde_json::from_slice(&fs::read(self.payload.join("release.json")).unwrap()).unwrap();
        for name in manifest
            .files
            .iter()
            .map(|f| f.path.as_str())
            .chain(["release.json", "release.sig"])
        {
            tar.append_path_with_name(self.payload.join(name), name)
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        archive
    }
    fn malicious_archive(&self, name: &str, kind: tar::EntryType, duplicate: bool) -> PathBuf {
        let archive = self.temp.path().join("dangerous.tar.gz");
        let encoder = GzEncoder::new(File::create(&archive).unwrap(), Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        let bytes = header.as_mut_bytes();
        bytes[..100].fill(0);
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o600);
        if kind.is_symlink() || kind.is_hard_link() {
            header.set_link_name("../../outside").unwrap();
        }
        header.set_cksum();
        tar.append(&header, Cursor::new([])).unwrap();
        if duplicate {
            tar.append(&header, Cursor::new([])).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        archive
    }
}

#[test]
fn signed_directory_and_complete_nested_payload_are_verified() {
    let f = Fixture::new();
    let manifest = verify_directory(&f.payload, &f.trust).unwrap();
    assert_eq!(manifest.files.len(), 6);
    fs::write(f.payload.join("tokemula-image"), b"tampered").unwrap();
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "release_integrity_failed"
    );
    f.resign(TARGET);
    fs::write(f.payload.join("unlisted"), b"extra").unwrap();
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "invalid_release_package"
    );
}

#[test]
fn wrong_signature_target_missing_required_and_unsafe_manifest_are_rejected() {
    let f = Fixture::new();
    fs::write(f.payload.join("release.sig"), STANDARD.encode([0; 64])).unwrap();
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "release_signature_invalid"
    );
    f.resign("wrong-target");
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "release_target_mismatch"
    );
    f.resign(TARGET);
    let mut manifest = verify_directory(&f.payload, &f.trust).unwrap();
    manifest.files.retain(|f| f.path != "LICENSE");
    f.sign_manifest(&manifest);
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "invalid_release_package"
    );
    manifest.files[0].path = "../outside".into();
    f.sign_manifest(&manifest);
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "unsafe_release_path"
    );
}

#[test]
fn signed_update_verifies_identity_before_metadata_and_rejects_credentials_or_wrong_target() {
    let f = Fixture::new();
    let archive = f.normal_archive();
    let update = f.update(&archive);
    let signed = |update: &ReleaseUpdate| {
        let payload = serde_json::to_vec(update).unwrap();
        serde_json::to_vec(&SignedEnvelope {
            payload: STANDARD.encode(&payload),
            signature: STANDARD.encode(f.signer.sign(&payload).to_bytes()),
        })
        .unwrap()
    };
    assert_eq!(verify_update(&signed(&update), &f.trust).unwrap(), update);
    let mut bad = update.clone();
    bad.target = "wrong-target".into();
    assert_eq!(
        verify_update(&signed(&bad), &f.trust).unwrap_err().code,
        "release_target_mismatch"
    );
    bad = update;
    bad.archive_url = "https://user:password@example.com/package".into();
    assert_eq!(
        verify_update(&signed(&bad), &f.trust).unwrap_err().code,
        "invalid_update_url"
    );
    let mut envelope: SignedEnvelope = serde_json::from_slice(&signed(&bad)).unwrap();
    envelope.signature = STANDARD.encode([0; 64]);
    assert_eq!(
        verify_update(&serde_json::to_vec(&envelope).unwrap(), &f.trust)
            .unwrap_err()
            .code,
        "release_signature_invalid"
    );
}

#[test]
fn bounded_archive_unpacks_exact_payload_and_rejects_damage_or_overwrite() {
    let f = Fixture::new();
    let archive = f.normal_archive();
    let update = f.update(&archive);
    let destination = f.temp.path().join("stage");
    assert_eq!(
        unpack_verified(&archive, &destination, &f.trust, &update)
            .unwrap()
            .version,
        update.version
    );
    assert_eq!(
        fs::read(destination.join("THIRD-PARTY/MIT.txt")).unwrap(),
        fs::read(f.payload.join("THIRD-PARTY/MIT.txt")).unwrap()
    );
    assert_eq!(
        unpack_verified(&archive, &destination, &f.trust, &update)
            .unwrap_err()
            .code,
        "release_destination_exists"
    );
    fs::write(&archive, b"damage").unwrap();
    assert_eq!(
        unpack_verified(&archive, &f.temp.path().join("new"), &f.trust, &update)
            .unwrap_err()
            .code,
        "release_integrity_failed"
    );
    assert!(!f.temp.path().join("new").exists());
}

#[test]
fn archive_paths_links_and_duplicates_never_escape_or_replace_destination() {
    let f = Fixture::new();
    for (name, kind, duplicate) in [
        ("../outside", tar::EntryType::Regular, false),
        ("/outside", tar::EntryType::Regular, false),
        ("C:\\outside", tar::EntryType::Regular, false),
        ("symlink", tar::EntryType::Symlink, false),
        ("hardlink", tar::EntryType::Link, false),
        ("same", tar::EntryType::Regular, true),
    ] {
        let archive = f.malicious_archive(name, kind, duplicate);
        let update = f.update(&archive);
        let stage = f.temp.path().join("stage");
        assert!(
            unpack_verified(&archive, &stage, &f.trust, &update).is_err(),
            "dangerous case should fail"
        );
        assert!(!stage.exists());
        assert!(!f.temp.path().join("outside").exists());
    }
}

#[test]
fn archive_limits_and_manifest_duplicates_fail_before_activation() {
    let f = Fixture::new();
    let mut manifest = verify_directory(&f.payload, &f.trust).unwrap();
    manifest.files.push(manifest.files[0].clone());
    f.sign_manifest(&manifest);
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "invalid_release_package"
    );
    manifest.files.pop();
    manifest.files[0].size = MAX_PACKAGE_BYTES + 1;
    f.sign_manifest(&manifest);
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "release_too_large"
    );
    let mut update = ReleaseUpdate {
        schema_version: 1,
        version: "0.1.0".into(),
        target: TARGET.into(),
        archive_url: "https://example.com/a".into(),
        archive_sha256: "0".repeat(64),
        archive_size: MAX_PACKAGE_BYTES + 1,
    };
    assert_eq!(
        validate_update(&update).unwrap_err().code,
        "invalid_release_package"
    );
    update.archive_size = 1;
    update.archive_url = "http://example.com/a".into();
    assert_eq!(
        validate_update(&update).unwrap_err().code,
        "invalid_update_url"
    );
}

#[test]
fn owned_verification_preserves_unlisted_user_files_but_rejects_modified_payload() {
    let f = Fixture::new();
    fs::create_dir(f.payload.join("user-added")).unwrap();
    fs::write(f.payload.join("user-added/notes.txt"), b"user content").unwrap();
    assert_eq!(
        verify_directory(&f.payload, &f.trust).unwrap_err().code,
        "invalid_release_package"
    );
    let manifest = verify_owned_directory(&f.payload, &f.trust).unwrap();
    assert!(
        !manifest
            .files
            .iter()
            .any(|file| file.path == "user-added/notes.txt")
    );
    fs::write(f.payload.join("tokemula-image"), b"modified program").unwrap();
    assert_eq!(
        verify_owned_directory(&f.payload, &f.trust)
            .unwrap_err()
            .code,
        "release_integrity_failed"
    );
    #[cfg(unix)]
    {
        fs::remove_file(f.payload.join("user-added/notes.txt")).unwrap();
        std::os::unix::fs::symlink("../../outside", f.payload.join("user-added/link")).unwrap();
        assert_eq!(
            verify_owned_directory(&f.payload, &f.trust)
                .unwrap_err()
                .code,
            "unsafe_release_path"
        );
    }
}

#[test]
fn removal_retries_authenticate_retained_metadata_and_only_verify_remaining_payload() {
    let f = Fixture::new();
    let metadata = f.temp.path().join("authenticated-metadata");
    fs::create_dir(&metadata).unwrap();
    for name in ["release.json", "release.sig"] {
        fs::copy(f.payload.join(name), metadata.join(name)).unwrap();
    }
    let mcp_original = fs::read(f.payload.join("tokemula-image-mcp")).unwrap();
    fs::remove_file(f.payload.join("tokemula-image")).unwrap();
    fs::remove_file(f.payload.join("release.json")).unwrap();
    fs::remove_file(f.payload.join("release.sig")).unwrap();
    fs::write(f.payload.join("user-notes.txt"), b"preserve user addition").unwrap();
    let manifest = verify_removal_directory(&f.payload, &metadata, &f.trust).unwrap();
    assert_eq!(manifest.files.len(), 6);
    assert!(
        !manifest
            .files
            .iter()
            .any(|file| file.path == "user-notes.txt")
    );
    fs::write(f.payload.join("tokemula-image-mcp"), b"changed remainder").unwrap();
    assert_eq!(
        verify_removal_directory(&f.payload, &metadata, &f.trust)
            .unwrap_err()
            .code,
        "release_integrity_failed"
    );
    fs::write(f.payload.join("tokemula-image-mcp"), &mcp_original).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", f.payload.join("user-link")).unwrap();
        assert_eq!(
            verify_removal_directory(&f.payload, &metadata, &f.trust)
                .unwrap_err()
                .code,
            "unsafe_release_path"
        );
        fs::remove_file(f.payload.join("user-link")).unwrap();
    }
    let signature = fs::read(metadata.join("release.sig")).unwrap();
    fs::write(metadata.join("release.sig"), STANDARD.encode([0; 64])).unwrap();
    assert_eq!(
        verify_removal_directory(&f.payload, &metadata, &f.trust)
            .unwrap_err()
            .code,
        "release_signature_invalid"
    );
    fs::write(metadata.join("release.sig"), signature).unwrap();
    fs::remove_dir_all(&f.payload).unwrap();
    assert_eq!(
        verify_removal_directory(&f.payload, &metadata, &f.trust).unwrap(),
        manifest
    );
}
