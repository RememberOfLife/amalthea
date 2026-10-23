use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs() as i64
}
pub fn secret() -> String {
    let mut bytes = [0u8; 64];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn id() -> String {
    hash(secret())[..32].to_owned()
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}
pub fn valid_username(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && ![
            "api", "assets", "res", "s", "register", "login", "health", "favicon", "index",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
}
pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub quota_bytes: i64,
}
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct FileRecord {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub size: i64,
    pub sha256: String,
    pub disk_stamp: String,
    pub status: String,
    pub processing_error: Option<String>,
    pub expected_sha256: Option<String>,
    pub cleanup_upload: bool,
    pub created_at: i64,
}
// SQLite names normally contain the literal UTF-8 filename. A NUL prefix (which
// cannot occur in a filesystem name) reserves an unambiguous, reversible encoding
// for non-UTF-8 names. This is a database key only; never rename files on import.
pub fn filename_key(name: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    name.to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("\0{}", URL_SAFE_NO_PAD.encode(name.as_bytes())))
}
impl FileRecord {
    pub fn disk_name(&self) -> crate::error::Result<std::ffi::OsString> {
        use std::os::unix::ffi::OsStringExt;
        match self.name.strip_prefix('\0') {
            Some(encoded) => URL_SAFE_NO_PAD
                .decode(encoded)
                .map(std::ffi::OsString::from_vec)
                .map_err(crate::error::Error::internal),
            None => Ok(self.name.clone().into()),
        }
    }
    pub fn require_ready(&self) -> crate::error::Result<()> {
        if self.status != "ready" {
            return Err(crate::error::Error::conflict("File is not ready"));
        }
        Ok(())
    }
    pub fn require_complete(&self) -> crate::error::Result<()> {
        self.require_ready()?;
        if valid_name(&self.name) {
            Ok(())
        } else {
            Err(crate::error::Error::conflict(
                "File is incomplete: fix its filename first",
            ))
        }
    }
}
impl Serialize for FileRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let name = self
            .disk_name()
            .map_err(|error| serde::ser::Error::custom(error.1))?;
        let mut record = serializer.serialize_struct("FileRecord", 9)?;
        record.serialize_field("status", &self.status)?;
        record.serialize_field("processing_error", &self.processing_error)?;
        record.serialize_field("id", &self.id)?;
        record.serialize_field("user_id", &self.user_id)?;
        record.serialize_field("name", &name.to_string_lossy())?;
        record.serialize_field("size", &self.size)?;
        record.serialize_field("sha256", &self.sha256)?;
        record.serialize_field("created_at", &self.created_at)?;
        record.serialize_field("incomplete", &!valid_name(&self.name))?;
        record.end()
    }
}
#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct Upload {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub size: i64,
    pub sha256: String,
    pub offset: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct LinkRecord {
    pub id: String,
    pub label: String,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub created_at: i64,
}
