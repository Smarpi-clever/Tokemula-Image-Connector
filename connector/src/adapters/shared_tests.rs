use super::*;

#[test]
#[cfg(unix)]
fn snapshot_is_private_even_before_ownership_is_committed() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("new state");
    let snapshot = save_snapshot(&state, Some(b"synthetic configuration")).unwrap();
    for directory in [&state, &state.join("snapshots")] {
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(snapshot).unwrap(), b"synthetic configuration");
    assert!(!state.join("ownership.json").exists());
}

#[test]
fn legacy_record_keeps_its_shape_and_shared_skill_survives_until_last_remove() {
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("shared/SKILL.md");
    write_private(&skill, SKILL_CONTENT.as_bytes()).unwrap();
    let original = json!({"installations":[{
        "harness":"codex","scope":"user","config_path":root.path().join("config.toml"),
        "skill_path":skill,"entry":{"command":"/absolute/mcp"},
        "skill":SKILL_CONTENT,"snapshot":root.path().join("snapshot.private")
    }]});
    let mut state: State = serde_json::from_value(original.clone()).unwrap();
    assert!(state.installations[0].skill_owned);
    assert_eq!(serde_json::to_value(&state).unwrap(), original);

    let mut reference = state.installations[0].clone();
    reference.harness = ManagedHarness::OpenCode;
    reference.config_path = root.path().join("opencode.jsonc");
    reference.skill_owned = false;
    state.installations.push(reference);
    let codex = state.installations.remove(0);
    release_skill(&mut state, &codex).unwrap();
    assert_eq!(fs::read_to_string(&skill).unwrap(), SKILL_CONTENT);
    assert!(state.installations[0].skill_owned);
    write_state(root.path(), &state).unwrap();

    let mut restored = read_state(root.path()).unwrap();
    let opencode = restored.installations.remove(0);
    release_skill(&mut restored, &opencode).unwrap();
    assert!(!skill.exists());
}

#[test]
fn borrowed_skill_removal_preserves_its_other_owner_and_unmanaged_files() {
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("shared/SKILL.md");
    write_private(&skill, SKILL_CONTENT.as_bytes()).unwrap();
    let note = root.path().join("shared/user-note.txt");
    fs::write(&note, "later user content").unwrap();
    let borrowed = Ownership {
        harness: ManagedHarness::OpenCode,
        scope: Scope::User,
        config_path: root.path().join("opencode.jsonc"),
        skill_path: skill.clone(),
        entry: json!({}),
        skill: SKILL_CONTENT.to_owned(),
        snapshot: root.path().join("snapshot.private"),
        skill_owned: false,
        context: None,
    };
    let mut owner = borrowed.clone();
    owner.harness = ManagedHarness::ClaudeCode;
    owner.skill_owned = true;
    let mut state = State {
        installations: vec![owner],
    };
    release_skill(&mut state, &borrowed).unwrap();
    assert!(skill.exists());
    assert!(state.installations[0].skill_owned);
    let last = state.installations.remove(0);
    release_skill(&mut state, &last).unwrap();
    remove_empty_parent(&skill);
    assert!(!skill.exists());
    assert_eq!(fs::read_to_string(note).unwrap(), "later user content");
}

#[test]
fn interrupted_managed_update_restores_only_its_entry_skill_and_references() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let config = root.join("config.toml");
    let skill = root.join("shared/SKILL.md");
    write_private(
        &config,
        b"# keep comment\nmodel = 'original'\n[mcp_servers.tokemula-image]\ncommand = '/old/mcp'\n",
    )
    .unwrap();
    write_private(&skill, b"old skill").unwrap();
    let old = Ownership {
        harness: ManagedHarness::Codex,
        scope: Scope::User,
        config_path: config.clone(),
        skill_path: skill.clone(),
        entry: json!({"command":"/old/mcp"}),
        skill: "old skill".into(),
        snapshot: root.join("install.private"),
        skill_owned: true,
        context: None,
    };
    let mut reference = old.clone();
    reference.harness = ManagedHarness::OpenCode;
    reference.config_path = root.join("other.jsonc");
    reference.skill_owned = false;
    let before = State {
        installations: vec![old.clone(), reference],
    };
    write_state(root, &before).unwrap();
    let mut after = before.clone();
    after.installations[0].entry = json!({"command":"/new/mcp"});
    for record in &mut after.installations {
        record.skill = "new skill".into();
    }
    let snapshot = save_snapshot(root, None).unwrap();
    let journal = managed::UpdateJournal {
        before: before.clone(),
        after,
        index: 0,
        snapshot,
    };
    write_private(
        &root.join("managed-update.private.json"),
        &serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    // These are the actual files left when a process exits after replacement but before the state commit.
    write_private(&config, b"# keep comment\nmodel = 'original'\nlater_user_field = 'retained'\n[mcp_servers.tokemula-image]\ncommand = '/new/mcp'\n").unwrap();
    write_private(&skill, b"new skill").unwrap();
    recover_owned_updates(root).unwrap();
    assert_eq!(fs::read(&skill).unwrap(), b"old skill");
    assert_eq!(
        serde_json::to_value(read_state(root).unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    let actual = fs::read_to_string(&config).unwrap();
    assert!(
        actual.contains("# keep comment")
            && actual.contains("later_user_field = 'retained'")
            && actual.contains("/old/mcp")
    );
    assert!(!root.join("managed-update.private.json").exists());
}

#[test]
fn empty_owned_cleanup_preserves_preferences_tasks_and_unknown_user_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write_state(root, &State::default()).unwrap();
    for name in ["snapshots", "tmp", "deepseek-recovery", "deepseek-packages"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    fs::write(root.join("user-note.txt"), b"keep user note").unwrap();
    fs::write(root.join("settings.json"), b"nonsecret preferences").unwrap();
    write_private(&root.join("tasks/task.json"), b"recovery reference").unwrap();
    cleanup_empty_owned_state(root).unwrap();
    assert!(!root.join("ownership.json").exists());
    assert!(!root.join("snapshots").exists());
    assert!(!root.join("tmp").exists());
    assert_eq!(
        fs::read(root.join("user-note.txt")).unwrap(),
        b"keep user note"
    );
    assert_eq!(
        fs::read(root.join("settings.json")).unwrap(),
        b"nonsecret preferences"
    );
    assert_eq!(
        fs::read(root.join("tasks/task.json")).unwrap(),
        b"recovery reference"
    );
    write_private(
        &root.join("ownership.json"),
        b"{\"installations\":[],\"user_note\":\"retained\"}",
    )
    .unwrap();
    fs::create_dir(root.join("snapshots")).unwrap();
    fs::write(
        root.join("snapshots/user-file.txt"),
        b"unknown snapshot neighbor",
    )
    .unwrap();
    cleanup_empty_owned_state(root).unwrap();
    assert!(root.join("ownership.json").exists());
    assert_eq!(
        fs::read(root.join("snapshots/user-file.txt")).unwrap(),
        b"unknown snapshot neighbor"
    );
}

#[test]
fn empty_owned_cleanup_keeps_metadata_while_recovery_or_targets_remain() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write_state(root, &State::default()).unwrap();
    write_private(
        &root.join("managed-update.private.json"),
        b"pending transaction",
    )
    .unwrap();
    assert_eq!(
        cleanup_empty_owned_state(root).unwrap_err().code,
        "pending_recovery"
    );
    assert!(root.join("ownership.json").exists());
    fs::remove_file(root.join("managed-update.private.json")).unwrap();
    write_private(
        &root.join("deepseek-recovery/web/remove.json"),
        b"pending native operation",
    )
    .unwrap();
    assert_eq!(
        cleanup_empty_owned_state(root).unwrap_err().code,
        "pending_recovery"
    );
    assert!(root.join("ownership.json").exists());
    fs::remove_file(root.join("deepseek-recovery/web/remove.json")).unwrap();
    let record = json!({"installations":[{"harness":"codex","scope":"user","config_path":root.join("config.toml"),"skill_path":root.join("SKILL.md"),"entry":{"command":"/owned/mcp"},"skill":"managed","snapshot":root.join("snapshots/one.private")}]});
    write_private(
        &root.join("ownership.json"),
        &serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        cleanup_empty_owned_state(root).unwrap_err().code,
        "owned_installations_remaining"
    );
    assert!(root.join("ownership.json").exists());
}
