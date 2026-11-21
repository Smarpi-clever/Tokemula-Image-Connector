use super::*;
use crate::tasks::{TaskRecord, TaskStatus, TaskStore};

#[test]
fn cleanup_preserves_unknown_user_files_and_rejects_substituted_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    fs::create_dir(&cache).unwrap();
    fs::write(cache.join("tic-image-123-AbCdEf123456.png"), b"synthetic").unwrap();
    fs::write(cache.join("my-saved-image.png"), b"user").unwrap();
    clear_managed_cache(&cache).unwrap();
    assert!(!cache.join("tic-image-123-AbCdEf123456.png").exists());
    assert_eq!(fs::read(cache.join("my-saved-image.png")).unwrap(), b"user");
    let tasks = tmp.path().join("tasks");
    let store = TaskStore {
        root: tasks.clone(),
    };
    store
        .save(&TaskRecord {
            task_id: "imgtask_known".into(),
            request_ref: Some("known-request".into()),
            selected_model: None,
            status: TaskStatus::Processing,
            created_at: 1,
            completed_at: None,
            expires_at: 2,
            result_expires_at: None,
            observed_at: 1,
        })
        .unwrap();
    fs::write(tasks.join("notes.txt"), b"user task notes").unwrap();
    purge_task_records(&tasks).unwrap();
    assert!(!tasks.join("imgtask_known.json").exists());
    assert!(tasks.join("notes.txt").exists());
    #[cfg(unix)]
    {
        let linked = tmp.path().join("linked");
        std::os::unix::fs::symlink(&cache, &linked).unwrap();
        assert!(clear_managed_cache(&linked).is_err());
        assert!(cache.join("my-saved-image.png").exists());
    }
}
#[test]
fn stable_guidance_requires_actual_owned_layout_and_quotes_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("install 中文 space");
    let executable = root.join("releases/0.1.0/tokemula-image");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::write(&executable, b"synthetic program").unwrap();
    fs::write(root.join(STATE), b"{}").unwrap();
    assert!(installation_root_for(&executable).is_none());
    let state = Installation {
        schema_version: 1,
        product: PRODUCT.into(),
        current: "0.1.0".into(),
        previous: None,
        update_url: None,
        public_key: "synthetic".into(),
        adapter_state: temp.path().join("state"),
        data_paths: DataPaths {
            tasks: temp.path().join("tasks"),
            cache: temp.path().join("cache"),
        },
        versions: vec!["0.1.0".into()],
    };
    write_json(&root.join(STATE), &state).unwrap();
    assert_eq!(installation_root_for(&executable), Some(root.clone()));
    assert!(shell_quote(&root).starts_with('\''));
    assert!(shell_quote(&root).ends_with('\''));
}

#[test]
fn purge_only_deletes_canonical_complete_current_records() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let tmp = tempfile::tempdir().unwrap();
    let tasks = tmp.path().join("tasks");
    let store = TaskStore {
        root: tasks.clone(),
    };
    fs::create_dir_all(&tasks).unwrap();
    let note = tasks.join("imgtask_owned.txt");
    fs::write(&note, b"user troubleshooting note").unwrap();
    store
        .save(&TaskRecord {
            task_id: "imgtask_owned".into(),
            request_ref: Some("known-request".into()),
            selected_model: None,
            status: TaskStatus::Processing,
            created_at: 1,
            completed_at: None,
            expires_at: 2,
            result_expires_at: None,
            observed_at: 1,
        })
        .unwrap();
    let mut annotated: Value =
        serde_json::from_slice(&fs::read(tasks.join("imgtask_owned.json")).unwrap()).unwrap();
    annotated["task_id"] = json!("imgtask_annotated");
    annotated["note"] = json!("user extension");
    fs::write(
        tasks.join("imgtask_annotated.json"),
        serde_json::to_vec(&annotated).unwrap(),
    )
    .unwrap();
    let batches = tasks.join("batches");
    fs::create_dir(&batches).unwrap();
    let key = "canonical-batch-key";
    let valid = json!({"record_type":"image_batch_v1","idempotency_key":key,"task_name":"stable-name","selected_model":"sample-fast","image_size":"1K","response_mime_type":"image/png","submission":"accepted","batch_id":"batch-owned","status":"queued","observed_at":1});
    let canonical = batches.join(format!("{}.json", URL_SAFE_NO_PAD.encode(key)));
    fs::write(&canonical, serde_json::to_vec(&valid).unwrap()).unwrap();
    let unknown = batches.join("user-notes.json");
    fs::write(&unknown,serde_json::to_vec(&json!({"record_type":"image_batch_v1","idempotency_key":"support-case","note":"user note"})).unwrap()).unwrap();
    let wrong = batches.join(format!("{}.json", URL_SAFE_NO_PAD.encode("different-key")));
    fs::write(&wrong, serde_json::to_vec(&valid).unwrap()).unwrap();
    let extra_key = "user-annotated-key";
    let mut extended = valid.clone();
    extended["idempotency_key"] = json!(extra_key);
    extended["note"] = json!("user extension");
    let extra = batches.join(format!("{}.json", URL_SAFE_NO_PAD.encode(extra_key)));
    fs::write(&extra, serde_json::to_vec(&extended).unwrap()).unwrap();
    purge_task_records(&tasks).unwrap();
    assert!(note.exists());
    assert!(!tasks.join("imgtask_owned.json").exists());
    assert!(tasks.join("imgtask_annotated.json").exists());
    assert!(!canonical.exists());
    assert!(unknown.exists());
    assert!(wrong.exists());
    assert!(extra.exists());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicit signed release directory and test public trust; never fetches a release"]
async fn initial_setup_recovery_preflights_all_links_and_normal_retry_succeeds() {
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;
    let release =
        PathBuf::from(std::env::var_os("D2_AUDIT_RELEASE").expect("explicit signed fixture"));
    let trust = PathBuf::from(std::env::var_os("D2_AUDIT_TRUST").expect("explicit test trust"));
    assert!(release.is_absolute() && trust.is_absolute());
    let manifest = release_package::verify_directory(&release, &trust).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // Each of the four entry slots is changed independently, including slots after valid ones.
    for changed in [
        "current",
        "recover",
        "bin/tokemula-image",
        "bin/tokemula-image-mcp",
    ] {
        let root = tmp.path().join(changed.replace('/', "-"));
        let state = tmp
            .path()
            .join(format!("state-{}", changed.replace('/', "-")));
        let installer = Installer::new(root.clone(), state.clone())
            .unwrap()
            .with_data_paths(DataPaths {
                tasks: tmp.path().join("tasks"),
                cache: tmp.path().join("cache"),
            })
            .unwrap()
            .with_test_hook(Arc::new(|phase| {
                if phase == "journal_prepared" {
                    panic!("bounded initial setup interruption")
                }
            }));
        assert!(
            AssertUnwindSafe(installer.setup(&release, &trust, None))
                .catch_unwind()
                .await
                .is_err()
        );
        let user = tmp
            .path()
            .join(format!("user-{}", changed.replace('/', "-")));
        fs::write(&user, b"foreign program").unwrap();
        let entry = root.join(changed);
        if fs::symlink_metadata(&entry).is_ok() {
            fs::remove_file(&entry).unwrap();
        }
        std::os::unix::fs::symlink(&user, &entry).unwrap();
        let names = [
            "current",
            "recover",
            "bin/tokemula-image",
            "bin/tokemula-image-mcp",
        ];
        let before: Vec<_> = names
            .iter()
            .map(|name| fs::read_link(root.join(name)).ok())
            .collect();
        let notice = root
            .join("releases")
            .join(&manifest.version)
            .join("THIRD-PARTY/NOTICE.md");
        let bytes = fs::read(&notice).unwrap();
        assert_eq!(
            installer.recover().await.unwrap_err().code,
            "installation_conflict"
        );
        for (name, target) in names.iter().zip(before) {
            assert_eq!(fs::read_link(root.join(name)).ok(), target);
        }
        assert_eq!(fs::read(&notice).unwrap(), bytes);
        assert!(root.join(JOURNAL).exists());
        assert!(user.exists());
    }
    let root = tmp.path().join("normal");
    let state = tmp.path().join("normal-state");
    let plain = || {
        Installer::new(root.clone(), state.clone())
            .unwrap()
            .with_data_paths(DataPaths {
                tasks: tmp.path().join("normal-tasks"),
                cache: tmp.path().join("normal-cache"),
            })
            .unwrap()
    };
    let interrupted = plain().with_test_hook(Arc::new(|phase| {
        if phase == "journal_prepared" {
            panic!("bounded normal initial interruption")
        }
    }));
    assert!(
        AssertUnwindSafe(interrupted.setup(&release, &trust, None))
            .catch_unwind()
            .await
            .is_err()
    );
    assert_eq!(plain().recover().await.unwrap()["status"], "not_installed");
    assert_eq!(
        plain().setup(&release, &trust, None).await.unwrap()["status"],
        "installed"
    );
}

#[test]
fn release_protocol_uses_target_version_and_preserves_legacy_restore() {
    let current = [
        "configure_tokemula",
        "submit_image_batch",
        "get_image_batch",
        "cancel_image_task",
        "get_image_task",
        "list_image_models",
        "generate_image",
        "edit_image",
    ];
    let mut legacy = current.to_vec();
    legacy.push("get_image_billing");
    for version in ["0.1.0-d2a.1", "0.1.0-d2b.1", "0.1.0-d4.2"] {
        assert!(release_tools_match(version, &legacy).unwrap());
        assert!(!release_tools_match(version, &current).unwrap());
    }
    for version in ["0.1.0-e1.1", "0.1.0-e1.2", "0.1.0"] {
        assert!(release_tools_match(version, &current).unwrap());
        assert!(!release_tools_match(version, &legacy).unwrap());
        let mut wrong = current;
        wrong[7] = "get_image_billing";
        assert!(!release_tools_match(version, &wrong).unwrap());
    }
    assert!(release_tools_match("not-semver", &current).is_err());
}
