//! One system credential entry is the authority for each operation's identity and preferences.

use crate::{
    http::{API_BASE, ImageClient},
    image_parameters::ImagePreferences,
    output::prepare_private_directory,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use zeroize::{Zeroize, Zeroizing};

const SERVICE: &str = "com.tokemula.image-connector";
const ACCOUNT: &str = "active-api-key";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPreferences {
    pub default_model: String,
    pub fallback_models: Vec<String>,
    pub auto_fallback: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Environment,
    SystemStore,
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsStatus {
    pub source: CredentialSource,
    pub availability: &'static str,
    pub preferences: Option<ModelPreferences>,
    pub preferences_status: &'static str,
    pub image_preferences: Option<ImagePreferences>,
    pub image_preferences_status: &'static str,
}

/// Deliberately neither Debug nor Serialize: its secret is handed only to the operation client.
pub struct OperationContext {
    pub api_key: Option<String>,
    pub preferences: Option<ModelPreferences>,
    pub image_preferences: Option<ImagePreferences>,
    pub source: CredentialSource,
}
impl Drop for OperationContext {
    fn drop(&mut self) {
        if let Some(key) = &mut self.api_key {
            key.zeroize();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsError {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for SettingsError {}
type Result<T> = std::result::Result<T, SettingsError>;
fn error(code: &'static str, message: &'static str) -> SettingsError {
    SettingsError { code, message }
}
fn storage_error() -> SettingsError {
    error(
        "settings_storage_failed",
        "无法保存模型设置；原配置已保留。",
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelOption {
    pub id: String,
    pub display_name: String,
    pub availability: String,
    pub capabilities: Vec<serde_json::Value>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    Ready,
    SubmissionRestricted,
    NoModels,
    NoImageAccess,
}
#[derive(Debug, Clone, Serialize)]
pub struct ValidationSummary {
    pub status: ValidationState,
    pub models: Vec<ModelOption>,
    pub can_submit: bool,
    pub reason_code: Option<String>,
}

pub struct ValidatedConfiguration {
    key: Zeroizing<String>,
    summary: ValidationSummary,
}
impl ValidatedConfiguration {
    pub fn summary(&self) -> &ValidationSummary {
        &self.summary
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct SaveOutcome {
    pub status: &'static str,
    pub preferences: ModelPreferences,
    pub active_source: CredentialSource,
    pub image_preferences: Option<ImagePreferences>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    api_key: String,
    preferences: ModelPreferences,
}
impl Drop for Envelope {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

trait SecretStore: Send + Sync {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn set(&self, value: &[u8]) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
struct NativeStore {
    service: String,
    account: String,
}
fn native_error(e: keyring::v1::Error) -> SettingsError {
    use keyring::v1::Error;
    match e {
        Error::NoDefaultStore => error(
            "credential_store_restart_required",
            "系统凭据服务首次初始化失败。服务恢复后，请重启当前配置进程再试。",
        ),
        Error::NoStorageAccess(_) => error(
            "credential_store_locked",
            "系统凭据库已锁定或拒绝访问。请解锁或允许访问后重试。",
        ),
        _ => error(
            "credential_store_unavailable",
            "系统凭据存储当前不可用。请恢复系统服务后重试。",
        ),
    }
}
impl NativeStore {
    fn entry(&self) -> Result<keyring::v1::Entry> {
        keyring::v1::Entry::new(&self.service, &self.account).map_err(native_error)
    }
}
impl SecretStore for NativeStore {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match self.entry()?.get_secret() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring::v1::Error::NoEntry) => Ok(None),
            Err(e) => Err(native_error(e)),
        }
    }
    fn set(&self, value: &[u8]) -> Result<()> {
        self.entry()?.set_secret(value).map_err(native_error)
    }
    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
            Err(e) => Err(native_error(e)),
        }
    }
}

struct Inner {
    directory: PathBuf,
    image_preferences_path: PathBuf,
    store: Arc<dyn SecretStore>,
    base: String,
    operation: Mutex<()>,
}
#[derive(Clone)]
pub struct SettingsService {
    inner: Arc<Inner>,
}
impl SettingsService {
    pub fn for_current_user() -> Result<Self> {
        #[cfg(windows)]
        let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let root = std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config"));
        let root = root
            .filter(|p| p.is_absolute())
            .ok_or_else(|| error("settings_path_unavailable", "无法确定当前用户的配置目录。"))?;
        let data_paths =
            crate::distribution::DataPaths::for_current_process().map_err(|_| storage_error())?;
        Ok(Self::new(
            root.join("tokemula-image-connector"),
            Arc::new(NativeStore {
                service: SERVICE.into(),
                account: ACCOUNT.into(),
            }),
            API_BASE.into(),
        )
        .with_data_paths(&data_paths))
    }
    fn new(directory: PathBuf, store: Arc<dyn SecretStore>, base: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                image_preferences_path: directory.join("image-preferences.json"),
                directory,
                store,
                base,
                operation: Mutex::new(()),
            }),
        }
    }
    /// Bind defaults to the same explicit data inventory used by installation and purge.
    pub fn with_data_paths(&self, paths: &crate::distribution::DataPaths) -> Self {
        Self {
            inner: Arc::new(Inner {
                directory: self.inner.directory.clone(),
                image_preferences_path: paths.image_preferences_path(),
                store: self.inner.store.clone(),
                base: self.inner.base.clone(),
                operation: Mutex::new(()),
            }),
        }
    }
    /// Controlled fixture constructor: the service name and HTTP host cannot target real entries/services.
    #[doc(hidden)]
    pub fn for_isolated_tests(
        directory: PathBuf,
        namespace: &str,
        loopback_base: &str,
    ) -> Result<Self> {
        let url = reqwest::Url::parse(loopback_base).map_err(|_| storage_error())?;
        if !directory.is_absolute()
            || !namespace.starts_with("tokemula-image-test-")
            || namespace.len() < 24
            || url.scheme() != "http"
            || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
            || url.port().is_none()
        {
            return Err(error(
                "invalid_test_context",
                "测试构造需要隔离目录、专用凭据条目及 loopback 地址。",
            ));
        }
        Ok(Self::new(
            directory,
            Arc::new(NativeStore {
                service: namespace.into(),
                account: "synthetic-test-key".into(),
            }),
            loopback_base.into(),
        ))
    }
    /// Remove only this product's credential and known preference file.
    pub fn purge(&self) -> Result<()> {
        let path = self.settings_path();
        let image_path = self.image_preferences_path();
        if fs::symlink_metadata(&image_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(image_storage_error());
        }
        self.read_image_preferences()?;
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(storage_error());
        }
        self.logout()?;
        if path.exists() {
            self.read_preferences()?;
            fs::remove_file(path).map_err(|_| storage_error())?;
        }
        if image_path.exists() {
            fs::remove_file(image_path).map_err(|_| image_storage_error())?;
        }
        let _ = fs::remove_dir(&self.inner.directory);
        Ok(())
    }
    fn settings_path(&self) -> PathBuf {
        self.inner.directory.join("settings.json")
    }
    fn read_preferences(&self) -> Result<Option<ModelPreferences>> {
        match fs::read(self.settings_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| error("settings_invalid", "模型偏好文件格式无效，请重新配置。")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(storage_error()),
        }
    }
    pub fn image_preferences_path(&self) -> PathBuf {
        self.inner.image_preferences_path.clone()
    }
    pub fn read_image_preferences(&self) -> Result<Option<ImagePreferences>> {
        match fs::read(self.image_preferences_path()) {
            Ok(bytes) => {
                let preferences: ImagePreferences =
                    serde_json::from_slice(&bytes).map_err(|_| image_preferences_invalid())?;
                preferences
                    .validate()
                    .map_err(|_| image_preferences_invalid())?;
                Ok(Some(preferences))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(image_storage_error()),
        }
    }
    /// Revalidate the saved identity for explicit configuration without exposing its key.
    pub async fn validate_saved(&self) -> Result<Option<ValidatedConfiguration>> {
        if std::env::var_os("TOKEMULA_IMAGE_KEY").is_some() {
            return Ok(None);
        }
        let Some(envelope) = self.envelope()? else {
            return Ok(None);
        };
        self.validate(envelope.api_key.clone()).await.map(Some)
    }
    fn envelope(&self) -> Result<Option<Envelope>> {
        self.inner
            .store
            .get()?
            .map(|bytes| {
                let value: Envelope = serde_json::from_slice(&bytes).map_err(|_| {
                    error(
                        "credential_data_invalid",
                        "本产品凭据格式无效，请重新配置。",
                    )
                })?;
                if value.version != 1 {
                    return Err(error("credential_data_invalid", "本产品凭据版本不受支持。"));
                }
                Ok(value)
            })
            .transpose()
    }
    /// Reads one credential envelope, once; an in-flight operation never reloads it.
    pub fn snapshot(&self) -> Result<OperationContext> {
        let _guard = self.inner.operation.lock().expect("settings lock");
        if let Ok(key) = std::env::var("TOKEMULA_IMAGE_KEY") {
            return Ok(OperationContext {
                api_key: (!key.is_empty()).then_some(key),
                preferences: None,
                image_preferences: None,
                source: CredentialSource::Environment,
            });
        }
        if let Some(envelope) = self.envelope()? {
            return Ok(OperationContext {
                api_key: Some(envelope.api_key.clone()),
                preferences: Some(envelope.preferences.clone()),
                image_preferences: self.read_image_preferences()?,
                source: CredentialSource::SystemStore,
            });
        }
        Ok(OperationContext {
            api_key: None,
            preferences: self.read_preferences()?,
            image_preferences: self.read_image_preferences()?,
            source: CredentialSource::None,
        })
    }
    pub fn status(&self) -> Result<SettingsStatus> {
        let context = self.snapshot()?;
        let (preferences, preferences_status) = if context.source == CredentialSource::Environment {
            match self.read_preferences() {
                Ok(Some(prefs)) => (Some(prefs), "saved_not_applied_environment"),
                Ok(None) => (None, "not_configured"),
                Err(_) => (None, "invalid_saved_preferences"),
            }
        } else {
            let status = if context.api_key.is_some() {
                "active"
            } else if context.preferences.is_some() {
                "saved_without_credential"
            } else {
                "not_configured"
            };
            (context.preferences.clone(), status)
        };
        Ok(SettingsStatus {
            source: context.source,
            availability: if context.api_key.is_some() {
                "available"
            } else {
                "missing"
            },
            preferences,
            preferences_status,
            image_preferences: if context.source == CredentialSource::Environment {
                self.read_image_preferences()?
            } else {
                context.image_preferences.clone()
            },
            image_preferences_status: if context.source == CredentialSource::Environment {
                "saved_not_applied_environment"
            } else if context.image_preferences.is_some() && context.api_key.is_some() {
                "active"
            } else if context.image_preferences.is_some() {
                "saved_without_credential"
            } else {
                "model_default"
            },
        })
    }
    pub async fn validate(&self, key: String) -> Result<ValidatedConfiguration> {
        let key = Zeroizing::new(key);
        if key.is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
            return Err(error("invalid_key", "请输入有效的 API Key。"));
        }
        let client = ImageClient::build(
            &self.inner.base,
            Some(key.to_string()),
            Duration::from_secs(30),
        )
        .map_err(|_| error("invalid_key", "API Key 格式无效。"))?;
        let (catalog, visible) = client.catalog().await.map_err(|e| match e.code {
            "authentication_failed" => error("invalid_key", "API Key 无效，请检查后重试。"),
            "permission_denied" => error("image_access_denied", "当前 Key 没有图片访问权限。"),
            _ => error(
                "catalog_unavailable",
                "图片目录接口尚未可用，请稍后重试。原配置保留。",
            ),
        })?;
        let models = visible["models"]
            .as_array()
            .expect("validated catalog")
            .iter()
            .map(|m| ModelOption {
                id: m["id"].as_str().expect("validated model").into(),
                display_name: m["display_name"].as_str().expect("validated name").into(),
                availability: m["availability"]
                    .as_str()
                    .expect("validated availability")
                    .into(),
                capabilities: m["capabilities"]
                    .as_array()
                    .expect("validated capabilities")
                    .clone(),
            })
            .collect::<Vec<_>>();
        let can_submit = visible["can_submit"].as_bool().expect("validated catalog");
        let reason = catalog
            .reason_code()
            .filter(|code| {
                matches!(
                    *code,
                    "image_access_disabled"
                        | "insufficient_balance"
                        | "quota_exhausted"
                        | "subscription_unavailable"
                        | "subscription_limit_exceeded"
                        | "no_compatible_model"
                )
            })
            .map(str::to_owned);
        let status = if reason.as_deref() == Some("image_access_disabled") {
            ValidationState::NoImageAccess
        } else if models.is_empty() {
            ValidationState::NoModels
        } else if can_submit {
            ValidationState::Ready
        } else {
            ValidationState::SubmissionRestricted
        };
        Ok(ValidatedConfiguration {
            key,
            summary: ValidationSummary {
                status,
                models,
                can_submit,
                reason_code: reason,
            },
        })
    }
    pub fn save(
        &self,
        validated: &ValidatedConfiguration,
        preferences: ModelPreferences,
    ) -> Result<SaveOutcome> {
        self.save_configuration(validated, preferences, None)
    }
    pub fn save_configuration(
        &self,
        validated: &ValidatedConfiguration,
        preferences: ModelPreferences,
        image_preferences: Option<ImagePreferences>,
    ) -> Result<SaveOutcome> {
        let ids: HashSet<_> = validated
            .summary
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        let mut selected = HashSet::new();
        if !ids.contains(preferences.default_model.as_str())
            || preferences.default_model.is_empty()
            || !selected.insert(&preferences.default_model)
            || preferences
                .fallback_models
                .iter()
                .any(|id| !ids.contains(id.as_str()) || !selected.insert(id))
        {
            return Err(error(
                "invalid_model_preferences",
                "请选择当前 Key 目录中的默认模型和不重复的备用顺序。",
            ));
        }
        if let Some(image) = &image_preferences {
            validate_image_preferences(
                image,
                validated
                    .summary
                    .models
                    .iter()
                    .find(|m| m.id == preferences.default_model)
                    .expect("validated model"),
            )?;
        }
        let _guard = self.inner.operation.lock().expect("settings lock");
        self.read_image_preferences()?;
        let old = self.inner.store.get()?;
        let old_preferences = match fs::read(self.settings_path()) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(storage_error()),
        };
        let file = self.prepare_preferences(&preferences)?;
        let image_file = image_preferences
            .as_ref()
            .map(|p| self.prepare_image_preferences(p))
            .transpose()?;
        let envelope = Envelope {
            version: 1,
            api_key: validated.key.to_string(),
            preferences: preferences.clone(),
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&envelope).map_err(|_| storage_error())?);
        self.inner.store.set(&bytes)?;
        if file.persist(self.settings_path()).is_err() {
            let rollback = match &old {
                Some(value) => self.inner.store.set(value),
                None => self.inner.store.delete(),
            };
            return match rollback {
                Ok(()) => Err(storage_error()),
                Err(_) => Err(error(
                    "configuration_save_partial",
                    "保存未完成，系统凭据回退失败。请重新配置；不要重复提交图片。",
                )),
            };
        }
        if let Some(file) = image_file
            && file.persist(self.image_preferences_path()).is_err()
        {
            let rollback_key = match &old {
                Some(value) => self.inner.store.set(value),
                None => self.inner.store.delete(),
            };
            let rollback_settings = match old_preferences {
                Some(bytes) => self.prepare_bytes(&bytes).and_then(|file| {
                    file.persist(self.settings_path())
                        .map(|_| ())
                        .map_err(|_| storage_error())
                }),
                None => fs::remove_file(self.settings_path()).map_err(|_| storage_error()),
            };
            return if rollback_key.is_ok() && rollback_settings.is_ok() {
                Err(image_storage_error())
            } else {
                Err(error(
                    "configuration_save_partial",
                    "保存未完成，原配置回退失败。请用 status 核对后重新配置。",
                ))
            };
        }
        Ok(SaveOutcome {
            status: "saved",
            preferences,
            image_preferences,
            active_source: if std::env::var_os("TOKEMULA_IMAGE_KEY").is_some() {
                CredentialSource::Environment
            } else {
                CredentialSource::SystemStore
            },
        })
    }
    fn prepare_preferences(
        &self,
        preferences: &ModelPreferences,
    ) -> Result<tempfile::NamedTempFile> {
        self.prepare_json(preferences)
    }
    fn prepare_image_preferences(
        &self,
        preferences: &ImagePreferences,
    ) -> Result<tempfile::NamedTempFile> {
        let path = self.image_preferences_path();
        let parent = path.parent().ok_or_else(image_storage_error)?;
        prepare_private_directory(parent).map_err(|_| image_storage_error())?;
        let mut file =
            tempfile::NamedTempFile::new_in(parent).map_err(|_| image_storage_error())?;
        file.write_all(&serde_json::to_vec(preferences).map_err(|_| image_storage_error())?)
            .map_err(|_| image_storage_error())?;
        file.as_file()
            .sync_all()
            .map_err(|_| image_storage_error())?;
        Ok(file)
    }
    fn prepare_json(&self, value: &impl Serialize) -> Result<tempfile::NamedTempFile> {
        self.prepare_bytes(&serde_json::to_vec(value).map_err(|_| storage_error())?)
    }
    fn prepare_bytes(&self, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
        prepare_private_directory(&self.inner.directory).map_err(|_| storage_error())?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.inner.directory).map_err(|_| storage_error())?;
        file.write_all(bytes).map_err(|_| storage_error())?;
        file.as_file().sync_all().map_err(|_| storage_error())?;
        Ok(file)
    }
    pub fn logout(&self) -> Result<SettingsStatus> {
        let _guard = self.inner.operation.lock().expect("settings lock");
        if let Some(envelope) = self.envelope()? {
            self.prepare_preferences(&envelope.preferences)?
                .persist(self.settings_path())
                .map_err(|_| storage_error())?;
        }
        self.inner.store.delete()?;
        drop(_guard);
        self.status()
    }
    #[cfg(test)]
    pub(crate) fn in_memory_for_tests(directory: PathBuf, loopback_base: &str) -> Self {
        Self::new(
            directory,
            Arc::new(MemoryStore::default()),
            loopback_base.into(),
        )
    }
}

fn image_storage_error() -> SettingsError {
    error(
        "image_preferences_storage_failed",
        "无法读取或保存图片偏好，原文件已保留。",
    )
}
fn image_preferences_invalid() -> SettingsError {
    error(
        "image_preferences_invalid",
        "图片偏好文件格式无效，原文件已保留。请检查 image-preferences.json。",
    )
}
pub(crate) fn validate_image_preferences(
    preferences: &ImagePreferences,
    model: &ModelOption,
) -> Result<()> {
    crate::image_parameters::validate_preferences(
        preferences,
        &serde_json::json!({"capabilities":model.capabilities}),
    )
    .map_err(|message| error("invalid_image_preferences", message))
}

#[cfg(test)]
#[derive(Default)]
struct MemoryStore {
    value: Mutex<Option<Vec<u8>>>,
}
#[cfg(test)]
impl SecretStore for MemoryStore {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self
            .value
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| Zeroizing::new(v.clone())))
    }
    fn set(&self, value: &[u8]) -> Result<()> {
        *self.value.lock().unwrap() = Some(value.to_vec());
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
