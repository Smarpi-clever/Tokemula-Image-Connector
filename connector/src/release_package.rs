//! Signed release payloads and bounded extraction; lifecycle activation belongs to the installer.

use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const TARGET: &str = env!("TOKEMULA_BUILD_TARGET");
pub const PRODUCT: &str = "tokemula-image-connector";
pub const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema_version: u32,
    pub product: String,
    pub version: String,
    pub target: String,
    pub files: Vec<ReleaseFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseUpdate {
    pub schema_version: u32,
    pub version: String,
    pub target: String,
    pub archive_url: String,
    pub archive_sha256: String,
    pub archive_size: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEnvelope {
    pub payload: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseError {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}
impl std::error::Error for ReleaseError {}
type Result<T> = std::result::Result<T, ReleaseError>;
fn error(code: &'static str, message: &'static str) -> ReleaseError {
    ReleaseError { code, message }
}
fn invalid() -> ReleaseError {
    error("invalid_release_package", "发行包格式或文件清单无效。")
}
fn storage() -> ReleaseError {
    error("release_storage_failed", "发行包文件无法读取或准备。")
}
fn integrity() -> ReleaseError {
    error(
        "release_integrity_failed",
        "发行包完整性校验失败，原版本保持。",
    )
}

fn ordinary_file(path: &Path, maximum: u64) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|_| storage())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return Err(invalid());
    }
    Ok(metadata)
}
fn bounded_read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    ordinary_file(path, maximum)?;
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| storage())?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| storage())?;
    if bytes.len() as u64 > maximum {
        return Err(invalid());
    }
    Ok(bytes)
}
fn verify_signature(bytes: &[u8], signature: &[u8], trust: &Path) -> Result<()> {
    let trust = bounded_read(trust, 1024)?;
    let trust = std::str::from_utf8(&trust)
        .map_err(|_| error("invalid_update_trust", "更新信任公钥格式无效。"))?;
    let key: [u8; 32] = STANDARD
        .decode(trust.trim())
        .ok()
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| error("invalid_update_trust", "更新信任公钥格式无效。"))?;
    let key = VerifyingKey::from_bytes(&key)
        .map_err(|_| error("invalid_update_trust", "更新信任公钥格式无效。"))?;
    let signature = Signature::from_slice(signature)
        .map_err(|_| error("release_signature_invalid", "发行签名无效，原版本保持。"))?;
    key.verify_strict(bytes, &signature)
        .map_err(|_| error("release_signature_invalid", "发行签名无效，原版本保持。"))
}

fn digest_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn safe_relative(value: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(error("unsafe_release_path", "发行包包含不安全路径或链接。"));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(error("unsafe_release_path", "发行包包含不安全路径或链接。"));
    }
    Ok(path.to_owned())
}
fn target_version(version: &str, target: &str) -> Result<()> {
    semver::Version::parse(version).map_err(|_| invalid())?;
    if target != TARGET {
        return Err(error(
            "release_target_mismatch",
            "发行包的系统或架构不适用于本机。",
        ));
    }
    Ok(())
}
fn validate_manifest(manifest: &ReleaseManifest) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.product != PRODUCT
        || manifest.files.is_empty()
        || manifest.files.len() > MAX_FILES
    {
        return Err(invalid());
    }
    target_version(&manifest.version, &manifest.target)?;
    let mut paths = HashSet::new();
    let mut total: u64 = 0;
    for file in &manifest.files {
        safe_relative(&file.path)?;
        if matches!(file.path.as_str(), "release.json" | "release.sig")
            || !paths.insert(file.path.as_str())
            || !digest_valid(&file.sha256)
        {
            return Err(invalid());
        }
        total = total.checked_add(file.size).ok_or_else(invalid)?;
        if total > MAX_PACKAGE_BYTES {
            return Err(error("release_too_large", "发行包超过允许的大小。"));
        }
    }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if !paths.contains(format!("tokemula-image{suffix}").as_str())
        || !paths.contains(format!("tokemula-image-mcp{suffix}").as_str())
        || !["LICENSE", "QUICKSTART.md", "THIRD-PARTY/NOTICE.md"]
            .iter()
            .all(|path| paths.contains(path))
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_update(update: &ReleaseUpdate) -> Result<()> {
    if update.schema_version != 1
        || !digest_valid(&update.archive_sha256)
        || update.archive_size == 0
        || update.archive_size > MAX_PACKAGE_BYTES
    {
        return Err(invalid());
    }
    target_version(&update.version, &update.target)?;
    let url = reqwest::Url::parse(&update.archive_url).map_err(|_| invalid())?;
    let local_http = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"));
    if !(url.scheme() == "https" || local_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error("invalid_update_url", "更新下载地址无效。"));
    }
    Ok(())
}

/// Verify publisher identity before interpreting the update payload.
pub fn verify_update(bytes: &[u8], trust: &Path) -> Result<ReleaseUpdate> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(invalid());
    }
    let envelope: SignedEnvelope = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let payload = STANDARD.decode(&envelope.payload).map_err(|_| invalid())?;
    let signature = STANDARD
        .decode(&envelope.signature)
        .map_err(|_| invalid())?;
    verify_signature(&payload, &signature, trust)?;
    let update: ReleaseUpdate = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    validate_update(&update)?;
    Ok(update)
}

fn directory_files(root: &Path, current: &Path, files: &mut HashSet<String>) -> Result<()> {
    for entry in fs::read_dir(current).map_err(|_| storage())? {
        let entry = entry.map_err(|_| storage())?;
        let kind = entry.file_type().map_err(|_| storage())?;
        if kind.is_symlink() {
            return Err(error("unsafe_release_path", "发行包包含不安全路径或链接。"));
        }
        if kind.is_dir() {
            directory_files(root, &entry.path(), files)?;
        } else if kind.is_file() {
            let relative = entry.path();
            let relative = relative.strip_prefix(root).map_err(|_| invalid())?;
            let relative = relative.to_str().ok_or_else(invalid)?.to_owned();
            #[cfg(windows)]
            let relative = relative.replace('\\', "/");
            safe_relative(&relative)?;
            if files.len() >= MAX_FILES + 2 || !files.insert(relative) {
                return Err(invalid());
            }
        } else {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Hashes are used only for files belonging to the signed release manifest.
pub fn sha256_file(path: &Path) -> Result<String> {
    ordinary_file(path, MAX_PACKAGE_BYTES)?;
    let mut hasher = Sha256::new();
    let mut input = File::open(path).map_err(|_| storage())?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(|_| storage())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn verify_directory(path: &Path, trust: &Path) -> Result<ReleaseManifest> {
    verify_directory_inner(path, trust, false)
}

/// Retain unlisted user files while authenticating every owned payload before removal.
pub fn verify_owned_directory(path: &Path, trust: &Path) -> Result<ReleaseManifest> {
    verify_directory_inner(path, trust, true)
}

fn authenticated_manifest(path: &Path, trust: &Path) -> Result<ReleaseManifest> {
    let root = fs::symlink_metadata(path).map_err(|_| storage())?;
    if !root.is_dir() || root.file_type().is_symlink() {
        return Err(invalid());
    }
    let manifest_bytes = bounded_read(&path.join("release.json"), MAX_MANIFEST_BYTES)?;
    let signature = bounded_read(&path.join("release.sig"), 1024)?;
    let signature = STANDARD
        .decode(
            std::str::from_utf8(&signature)
                .map_err(|_| invalid())?
                .trim(),
        )
        .map_err(|_| invalid())?;
    verify_signature(&manifest_bytes, &signature, trust)?;
    let manifest: ReleaseManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| invalid())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn verify_directory_inner(
    path: &Path,
    trust: &Path,
    allow_user_files: bool,
) -> Result<ReleaseManifest> {
    let manifest = authenticated_manifest(path, trust)?;
    let mut actual = HashSet::new();
    directory_files(path, path, &mut actual)?;
    let mut expected: HashSet<_> = manifest.files.iter().map(|f| f.path.clone()).collect();
    expected.extend(["release.json".to_owned(), "release.sig".to_owned()]);
    if !expected.is_subset(&actual) || (!allow_user_files && actual != expected) {
        return Err(invalid());
    }
    for file in &manifest.files {
        let file_path = path.join(safe_relative(&file.path)?);
        if ordinary_file(&file_path, MAX_PACKAGE_BYTES)?.len() != file.size
            || !sha256_file(&file_path)?.eq_ignore_ascii_case(&file.sha256)
        {
            return Err(integrity());
        }
    }
    Ok(manifest)
}

/// Resume removal against separately retained, signed metadata after some payloads are gone.
pub fn verify_removal_directory(
    payload: &Path,
    authenticated_metadata: &Path,
    trust: &Path,
) -> Result<ReleaseManifest> {
    let manifest = authenticated_manifest(authenticated_metadata, trust)?;
    match fs::symlink_metadata(payload) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(manifest),
        Err(_) => return Err(storage()),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(error("unsafe_release_path", "发行包包含不安全路径或链接。"));
        }
        Ok(metadata) if !metadata.is_dir() => return Err(invalid()),
        Ok(_) => {}
    }
    let mut actual = HashSet::new();
    directory_files(payload, payload, &mut actual)?;
    for file in &manifest.files {
        let path = payload.join(safe_relative(&file.path)?);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(storage()),
            Ok(_) => {}
        }
        if ordinary_file(&path, MAX_PACKAGE_BYTES)?.len() != file.size
            || !sha256_file(&path)?.eq_ignore_ascii_case(&file.sha256)
        {
            return Err(integrity());
        }
    }
    Ok(manifest)
}

pub fn unpack_verified(
    archive: &Path,
    destination: &Path,
    trust: &Path,
    expected: &ReleaseUpdate,
) -> Result<ReleaseManifest> {
    validate_update(expected)?;
    if ordinary_file(archive, MAX_PACKAGE_BYTES)?.len() != expected.archive_size
        || !sha256_file(archive)?.eq_ignore_ascii_case(&expected.archive_sha256)
    {
        return Err(integrity());
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(error(
            "release_destination_exists",
            "发行暂存目录已经存在，未覆盖。",
        ));
    }
    fs::create_dir_all(destination).map_err(|_| storage())?;
    let result = (|| {
        let decoded = flate2::read::GzDecoder::new(File::open(archive).map_err(|_| storage())?);
        let mut tar = tar::Archive::new(decoded);
        let mut names = HashSet::new();
        let mut total: u64 = 0;
        for entry in tar.entries().map_err(|_| invalid())? {
            let mut entry = entry.map_err(|_| invalid())?;
            let raw = entry
                .path()
                .map_err(|_| invalid())?
                .to_str()
                .ok_or_else(invalid)?
                .trim_end_matches('/')
                .to_owned();
            let relative = safe_relative(&raw)?;
            if names.len() >= MAX_FILES * 2 || !names.insert(relative.clone()) {
                return Err(invalid());
            }
            let kind = entry.header().entry_type();
            let output = destination.join(&relative);
            if kind.is_dir() {
                if entry.size() != 0 {
                    return Err(invalid());
                }
                fs::create_dir_all(output).map_err(|_| storage())?;
            } else if kind.is_file() {
                total = total.checked_add(entry.size()).ok_or_else(invalid)?;
                if total > MAX_PACKAGE_BYTES {
                    return Err(error("release_too_large", "发行包超过允许的大小。"));
                }
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent).map_err(|_| storage())?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&output)
                    .map_err(|_| storage())?;
                std::io::copy(&mut entry, &mut file).map_err(|_| invalid())?;
                file.flush().map_err(|_| storage())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let executable =
                        matches!(raw.as_str(), "tokemula-image" | "tokemula-image-mcp");
                    fs::set_permissions(
                        output,
                        fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
                    )
                    .map_err(|_| storage())?;
                }
            } else {
                return Err(error("unsafe_release_path", "发行包包含不安全路径或链接。"));
            }
        }
        let manifest = verify_directory(destination, trust)?;
        if manifest.version != expected.version || manifest.target != expected.target {
            return Err(error(
                "release_metadata_mismatch",
                "更新元数据与实际发行包不一致。",
            ));
        }
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

#[cfg(test)]
#[path = "release_package_tests.rs"]
mod tests;
