//! Product lifecycle operations reuse the adapter ownership journal.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ManagedContext {
    Standard {
        harness: Harness,
        scope: Scope,
        paths: AdapterPaths,
    },
    OpenCode {
        scope: Scope,
        paths: opencode::OpenCodePaths,
        overrides: opencode::OpenCodeOverrides,
    },
    DeepSeek {
        paths: deepseek::DeepSeekPaths,
    },
    WorkBuddy {
        paths: workbuddy::WorkBuddyPaths,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct OwnedInstallation {
    pub harness: String,
    pub scope: Scope,
    pub config_path: PathBuf,
    pub skill_path: PathBuf,
    pub executable: Option<PathBuf>,
    pub context_available: bool,
}

#[derive(Debug, Serialize)]
pub struct ManagedOperationResult {
    pub installation: OwnedInstallation,
    pub status: Option<String>,
    pub error: Option<AdapterError>,
}

#[derive(Debug, Serialize)]
pub struct ManagedOperationReport {
    pub completed: bool,
    pub items: Vec<ManagedOperationResult>,
}

fn name(harness: ManagedHarness) -> &'static str {
    match harness {
        ManagedHarness::Codex => "codex",
        ManagedHarness::ClaudeCode => "claude",
        ManagedHarness::OpenCode => "opencode",
        ManagedHarness::DeepSeek => "deepseek",
        ManagedHarness::WorkBuddy => "workbuddy",
    }
}
fn executable(record: &Ownership) -> Option<PathBuf> {
    let value = if record.harness == ManagedHarness::DeepSeek {
        let patch: Value =
            serde_json::from_str(record.entry["files"]["cordis.patch.yml"].as_str()?).ok()?;
        patch[0]["insert"][0]["config"]["command"]
            .as_str()?
            .to_owned()
    } else if record.harness == ManagedHarness::OpenCode {
        record.entry["command"][0].as_str()?.to_owned()
    } else {
        record.entry["command"].as_str()?.to_owned()
    };
    Some(value.into())
}
fn describe(record: &Ownership) -> OwnedInstallation {
    OwnedInstallation {
        harness: name(record.harness).into(),
        scope: record.scope,
        config_path: record.config_path.clone(),
        skill_path: record.skill_path.clone(),
        executable: executable(record),
        context_available: record.context.is_some(),
    }
}

pub fn owned_inventory(state_dir: &Path) -> Result<Vec<OwnedInstallation>> {
    Ok(read_state(state_dir)?
        .installations
        .iter()
        .map(describe)
        .collect())
}

/// Removes only empty, recognized adapter metadata after all external targets are removed.
pub fn cleanup_empty_owned_state(state_dir: &Path) -> Result<()> {
    match fs::symlink_metadata(state_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(conflict()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(storage_error()),
    }
    if !read_state(state_dir)?.installations.is_empty() {
        return Err(error(
            "owned_installations_remaining",
            "仍有受管接入未移除；归属资料和管理程序已保留。",
        ));
    }
    if read_optional(&journal_path(state_dir))?.is_some() {
        return Err(error(
            "pending_recovery",
            "受管更新仍有待恢复操作；归属资料已保留。",
        ));
    }
    let recovery = state_dir.join("deepseek-recovery");
    match fs::symlink_metadata(&recovery) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            for entry in fs::read_dir(&recovery).map_err(|_| storage_error())? {
                let entry = entry.map_err(|_| storage_error())?;
                if entry.file_type().map_err(|_| storage_error())?.is_dir() {
                    match fs::symlink_metadata(entry.path().join("remove.json")) {
                        Ok(_) => {
                            return Err(error(
                                "pending_recovery",
                                "DeepSeek 移除仍有待恢复记录；归属资料已保留。",
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Err(storage_error()),
                    }
                }
            }
        }
        Ok(_) => return Err(conflict()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(storage_error()),
    }
    let path = state_dir.join("ownership.json");
    if let Some(bytes) = read_optional(&path)? {
        let document: Value = serde_json::from_slice(&bytes).map_err(|_| configuration_error())?;
        // Unknown additions may be user content or a future schema; leave that file intact.
        if document.as_object().is_some_and(|object| {
            object.len() == 1
                && object
                    .get("installations")
                    .is_some_and(|value| value.as_array().is_some_and(Vec::is_empty))
        }) {
            fs::remove_file(&path).map_err(|_| storage_error())?;
        }
    }
    for directory in [
        state_dir.join("snapshots"),
        state_dir.join("tmp"),
        recovery,
        state_dir.join("deepseek-packages"),
        state_dir.to_path_buf(),
    ] {
        match fs::remove_dir(directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(_) => return Err(storage_error()),
        }
    }
    Ok(())
}

impl ManagedContext {
    fn with_executable(&self, executable: &Path) -> Self {
        let mut next = self.clone();
        match &mut next {
            Self::Standard { paths, .. } => paths.executable = executable.into(),
            Self::OpenCode { paths, .. } => paths.executable = executable.into(),
            Self::DeepSeek { paths } => paths.executable = executable.into(),
            Self::WorkBuddy { paths } => paths.executable = executable.into(),
        }
        next
    }
    fn state_dir(&self) -> &Path {
        match self {
            Self::Standard { paths, .. } => &paths.state_dir,
            Self::OpenCode { paths, .. } => &paths.state_dir,
            Self::DeepSeek { paths } => &paths.state_dir,
            Self::WorkBuddy { paths } => &paths.state_dir,
        }
    }
    fn validate(&self, record: &Ownership, root: &Path) -> Result<()> {
        if self.state_dir() != root {
            return Err(conflict());
        }
        let valid = match self {
            Self::Standard {
                harness,
                scope,
                paths,
            } => {
                let adapter = Adapter::new(*harness, *scope, paths.clone())?;
                record.harness == (*harness).into()
                    && record.scope == *scope
                    && record.config_path == adapter.config_path()
                    && record.skill_path == adapter.skill_path()
                    && adapter.check()? == InstallStatus::Installed
            }
            Self::OpenCode {
                scope,
                paths,
                overrides,
            } => {
                let adapter =
                    opencode::OpenCodeAdapter::new(*scope, paths.clone(), overrides.clone())?;
                let (config, skill) = adapter.managed_paths()?;
                record.harness == ManagedHarness::OpenCode
                    && record.scope == *scope
                    && record.config_path == config
                    && record.skill_path == skill
                    && adapter.check()? == InstallStatus::Installed
            }
            Self::DeepSeek { paths } => {
                let adapter = deepseek::DeepSeekAdapter::new(Scope::User, paths.clone())?;
                record.harness == ManagedHarness::DeepSeek
                    && record.scope == Scope::User
                    && record.config_path == adapter.config_path()
                    && record.skill_path == adapter.skill_path()
                    && adapter.check()? == InstallStatus::Installed
            }
            Self::WorkBuddy { paths } => {
                let adapter = workbuddy::WorkBuddyAdapter::new(Scope::User, paths.clone())?;
                record.harness == ManagedHarness::WorkBuddy
                    && record.scope == Scope::User
                    && record.config_path == adapter.config_path()
                    && record.skill_path == adapter.skill_path()
                    && matches!(
                        adapter.check()?,
                        workbuddy::WorkBuddyStatus::Installed
                            | workbuddy::WorkBuddyStatus::AwaitingConfirmation
                    )
            }
        };
        if valid { Ok(()) } else { Err(conflict()) }
    }
    fn update(&self) -> Result<String> {
        match self {
            Self::Standard {
                harness,
                scope,
                paths,
            } => Adapter::new(*harness, *scope, paths.clone())?
                .update()
                .and_then(status_text),
            Self::OpenCode {
                scope,
                paths,
                overrides,
            } => opencode::OpenCodeAdapter::new(*scope, paths.clone(), overrides.clone())?
                .update()
                .and_then(status_text),
            Self::DeepSeek { paths } => deepseek::DeepSeekAdapter::new(Scope::User, paths.clone())?
                .update()
                .and_then(status_text),
            Self::WorkBuddy { paths } => {
                workbuddy::WorkBuddyAdapter::new(Scope::User, paths.clone())?
                    .update()
                    .and_then(status_text)
            }
        }
    }
    fn remove(&self) -> Result<String> {
        match self {
            Self::Standard {
                harness,
                scope,
                paths,
            } => Adapter::new(*harness, *scope, paths.clone())?
                .remove()
                .and_then(status_text),
            Self::OpenCode {
                scope,
                paths,
                overrides,
            } => opencode::OpenCodeAdapter::new(*scope, paths.clone(), overrides.clone())?
                .remove()
                .and_then(status_text),
            Self::DeepSeek { paths } => deepseek::DeepSeekAdapter::new(Scope::User, paths.clone())?
                .remove()
                .and_then(status_text),
            Self::WorkBuddy { paths } => {
                workbuddy::WorkBuddyAdapter::new(Scope::User, paths.clone())?
                    .remove()
                    .and_then(status_text)
            }
        }
    }
}
fn status_text(value: impl Serialize) -> Result<String> {
    serde_json::to_value(value)
        .map_err(|_| configuration_error())?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(configuration_error)
}

fn context(record: &Ownership) -> Result<&ManagedContext> {
    record.context.as_ref().ok_or_else(|| {
        error(
            "installation_context_required",
            "旧受管记录缺少客户端运行上下文；请用原作用域的显式适配器更新或移除，管理程序已保留。",
        )
    })
}

/// Removes each recorded target; failures leave their ownership and the product recovery entry intact.
pub fn remove_owned(state_dir: &Path) -> Result<ManagedOperationReport> {
    recover_owned_updates(state_dir)?;
    let records = read_state(state_dir)?.installations;
    let mut items = Vec::with_capacity(records.len());
    for record in records {
        let result = context(&record).and_then(|ctx| {
            ctx.validate(&record, state_dir)?;
            ctx.remove()
        });
        let (status, error) = match result {
            Ok(status) => (Some(status), None),
            Err(error) => (None, Some(error)),
        };
        items.push(ManagedOperationResult {
            installation: describe(&record),
            status,
            error,
        });
    }
    Ok(ManagedOperationReport {
        completed: items.iter().all(|item| item.error.is_none()),
        items,
    })
}

/// Updates the set without leaving earlier successful targets on a different Skill generation.
pub fn update_owned(state_dir: &Path, new_executable: &Path) -> Result<ManagedOperationReport> {
    recover_owned_updates(state_dir)?;
    let records = read_state(state_dir)?.installations;
    let mut items = vec![];
    for record in &records {
        if let Err(error) = context(record).and_then(|ctx| ctx.validate(record, state_dir)) {
            items.push(ManagedOperationResult {
                installation: describe(record),
                status: None,
                error: Some(error),
            });
        }
    }
    if !items.is_empty() {
        return Ok(ManagedOperationReport {
            completed: false,
            items,
        });
    }
    for record in &records {
        let result = context(record)?.with_executable(new_executable).update();
        match result {
            Ok(status) => items.push(ManagedOperationResult {
                installation: describe(record),
                status: Some(status),
                error: None,
            }),
            Err(error) => {
                let mut failure = error;
                for prior in records.iter().rev() {
                    let current = read_state(state_dir)?;
                    if let Some(index) = current.installations.iter().position(|r| {
                        r.harness == prior.harness
                            && r.scope == prior.scope
                            && r.config_path == prior.config_path
                    }) && change_owned(
                        state_dir,
                        index,
                        prior.entry.clone(),
                        &prior.skill,
                        prior.context.clone(),
                    )
                    .is_err()
                    {
                        failure = error_for_recovery();
                    }
                }
                let rollback_status = if failure.code == "rollback_required" {
                    "recovery_required"
                } else {
                    "rolled_back"
                };
                for item in &mut items {
                    item.status = Some(rollback_status.into());
                }
                items.push(ManagedOperationResult {
                    installation: describe(record),
                    status: None,
                    error: Some(failure),
                });
                return Ok(ManagedOperationReport {
                    completed: false,
                    items,
                });
            }
        }
    }
    Ok(ManagedOperationReport {
        completed: true,
        items,
    })
}
fn error_for_recovery() -> AdapterError {
    error(
        "rollback_required",
        "受管更新失败且局部恢复未完成；原归属、快照和管理恢复程序保留。",
    )
}

fn current_entry(record: &Ownership) -> Result<Option<Value>> {
    if record.harness == ManagedHarness::DeepSeek {
        let Some(ManagedContext::DeepSeek { paths }) = &record.context else {
            return Err(conflict());
        };
        return deepseek::DeepSeekAdapter::new(Scope::User, paths.clone())?.entry();
    }
    if record.harness == ManagedHarness::OpenCode {
        return opencode::managed_entry(&record.config_path);
    }
    let bytes = read_optional(&record.config_path)?;
    if record.harness == ManagedHarness::Codex {
        let doc = parse_toml(bytes.as_deref())?;
        let Some(item) = doc
            .get("mcp_servers")
            .and_then(Item::as_table_like)
            .and_then(|table| table.get(REGISTRATION_NAME))
        else {
            return Ok(None);
        };
        let mut entry = DocumentMut::new();
        entry["entry"] = item.clone();
        let json: Value =
            toml_edit::de::from_str(&entry.to_string()).map_err(|_| configuration_error())?;
        return Ok(Some(json["entry"].clone()));
    }
    let doc = parse_json(bytes.as_deref())?;
    Ok(doc
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|map| map.get(REGISTRATION_NAME))
        .cloned())
}
fn patch_entry(record: &Ownership, entry: &Value) -> Result<()> {
    match record.harness {
        ManagedHarness::DeepSeek => {
            let Some(ManagedContext::DeepSeek { paths }) = &record.context else {
                return Err(conflict());
            };
            deepseek::DeepSeekAdapter::new(Scope::User, paths.clone())?.refresh_managed_files(entry)
        }
        ManagedHarness::OpenCode => opencode::patch_managed_entry(&record.config_path, Some(entry)),
        ManagedHarness::Codex => {
            let mut doc = parse_toml(read_optional(&record.config_path)?.as_deref())?;
            let table = doc["mcp_servers"]
                .as_table_like_mut()
                .and_then(|servers| servers.get_mut(REGISTRATION_NAME))
                .and_then(Item::as_table_like_mut)
                .ok_or_else(configuration_error)?;
            let decor = table
                .get("command")
                .and_then(Item::as_value)
                .ok_or_else(configuration_error)?
                .decor()
                .clone();
            let mut command =
                toml_edit::Value::from(entry["command"].as_str().ok_or_else(configuration_error)?);
            *command.decor_mut() = decor;
            table.insert("command", Item::Value(command));
            write_private(&record.config_path, doc.to_string().as_bytes())
        }
        _ => {
            let mut doc = parse_json(read_optional(&record.config_path)?.as_deref())?;
            doc["mcpServers"]
                .as_object_mut()
                .ok_or_else(configuration_error)?
                .insert(REGISTRATION_NAME.into(), entry.clone());
            write_private(
                &record.config_path,
                &serde_json::to_vec_pretty(&doc).map_err(|_| storage_error())?,
            )
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct UpdateJournal {
    pub(super) before: State,
    pub(super) after: State,
    pub(super) index: usize,
    pub(super) snapshot: PathBuf,
}
fn journal_path(root: &Path) -> PathBuf {
    root.join("managed-update.private.json")
}
fn same_record(a: &Ownership, b: &Ownership) -> Result<bool> {
    Ok(serde_json::to_value(a).map_err(|_| storage_error())?
        == serde_json::to_value(b).map_err(|_| storage_error())?)
}
fn same_identity(a: &Ownership, b: &Ownership) -> bool {
    a.harness == b.harness && a.scope == b.scope && a.config_path == b.config_path
}

/// Restores only matching old/new managed changes after an interrupted adapter update.
pub fn recover_owned_updates(root: &Path) -> Result<()> {
    let Some(bytes) = read_optional(&journal_path(root))? else {
        return Ok(());
    };
    let journal: UpdateJournal =
        serde_json::from_slice(&bytes).map_err(|_| configuration_error())?;
    if !root.is_absolute()
        || journal.before.installations.len() != journal.after.installations.len()
        || journal
            .before
            .installations
            .iter()
            .zip(&journal.after.installations)
            .any(|(old, new)| !same_identity(old, new))
        || journal.snapshot.parent() != Some(root.join("snapshots").as_path())
        || !journal
            .snapshot
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("configuration-") && name.ends_with(".private"))
    {
        return Err(conflict());
    }
    let mut old = journal
        .before
        .installations
        .get(journal.index)
        .ok_or_else(conflict)?
        .clone();
    let new = journal
        .after
        .installations
        .get(journal.index)
        .ok_or_else(conflict)?;
    if !same_identity(&old, new) || old.skill_path != new.skill_path {
        return Err(conflict());
    }
    if old.context.is_none() {
        old.context = new.context.clone();
    }
    let current = current_entry(&old)?;
    if old.harness == ManagedHarness::DeepSeek {
        let actual = current.as_ref().ok_or_else(conflict)?;
        if actual["dependency"] != old.entry["dependency"] {
            return Err(conflict());
        }
        for file in ["package.json", "cordis.patch.yml", "index.js"] {
            if actual["files"][file] != old.entry["files"][file]
                && actual["files"][file] != new.entry["files"][file]
            {
                return Err(conflict());
            }
        }
        if actual != &old.entry {
            patch_entry(&old, &old.entry)?;
        }
    } else if current.as_ref() == Some(&new.entry) && new.entry != old.entry {
        patch_entry(&old, &old.entry)?;
    } else if current.as_ref() != Some(&old.entry) {
        return Err(conflict());
    }
    let skill = read_optional(&old.skill_path)?.ok_or_else(conflict)?;
    if skill == new.skill.as_bytes() && new.skill != old.skill {
        write_private(&old.skill_path, old.skill.as_bytes())?;
    } else if skill != old.skill.as_bytes() {
        return Err(conflict());
    }
    let mut current = read_state(root)?;
    let mut changed = false;
    for (before, after) in journal
        .before
        .installations
        .iter()
        .zip(&journal.after.installations)
    {
        if same_record(before, after)? {
            continue;
        }
        let index = current
            .installations
            .iter()
            .position(|r| same_identity(r, before))
            .ok_or_else(conflict)?;
        if same_record(&current.installations[index], after)? {
            current.installations[index] = before.clone();
            changed = true;
        } else if !same_record(&current.installations[index], before)? {
            return Err(conflict());
        }
    }
    if changed {
        write_state(root, &current)?;
    }
    fs::remove_file(journal_path(root)).map_err(|_| storage_error())?;
    let _ = fs::remove_file(journal.snapshot);
    Ok(())
}

pub(super) fn change_owned(
    root: &Path,
    index: usize,
    entry: Value,
    skill: &str,
    context: Option<ManagedContext>,
) -> Result<InstallStatus> {
    recover_owned_updates(root)?;
    let before = read_state(root)?;
    let mut owned = before
        .installations
        .get(index)
        .ok_or_else(conflict)?
        .clone();
    if owned.context.is_none() {
        owned.context = context.clone();
    }
    if current_entry(&owned)?.as_ref() != Some(&owned.entry) {
        return Err(conflict());
    }
    let skill_bytes = read_optional(&owned.skill_path)?.ok_or_else(conflict)?;
    for reference in before
        .installations
        .iter()
        .filter(|record| record.skill_path == owned.skill_path)
    {
        if reference.skill.as_bytes() != skill_bytes {
            return Err(conflict());
        }
    }
    if owned.entry == entry && owned.skill == skill && before.installations[index].context.is_some()
    {
        return Ok(InstallStatus::AlreadyInstalled);
    }
    let snapshot = save_snapshot(root, read_optional(&owned.config_path)?.as_deref())?;
    let mut next = before.clone();
    next.installations[index].entry = entry.clone();
    next.installations[index].context = context.or_else(|| owned.context.clone());
    for record in next
        .installations
        .iter_mut()
        .filter(|r| r.skill_path == owned.skill_path)
    {
        record.skill = skill.into();
    }
    let journal = UpdateJournal {
        before,
        after: next.clone(),
        index,
        snapshot: snapshot.clone(),
    };
    write_private(
        &journal_path(root),
        &serde_json::to_vec(&journal).map_err(|_| storage_error())?,
    )?;
    let operation = (|| {
        if owned.entry != entry {
            patch_entry(&owned, &entry)?;
        }
        if owned.skill != skill {
            write_private(&owned.skill_path, skill.as_bytes())?;
        }
        write_state(root, &next)?;
        Ok(InstallStatus::Installed)
    })();
    if operation.is_err() {
        if recover_owned_updates(root).is_err() {
            return Err(error_for_recovery());
        }
    } else {
        fs::remove_file(journal_path(root)).map_err(|_| storage_error())?;
        let _ = fs::remove_file(snapshot);
    }
    operation
}
