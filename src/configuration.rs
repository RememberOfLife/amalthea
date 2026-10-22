use anyhow::{Context, bail};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub webroot: String,
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub static_dir: PathBuf,
    pub imprint: Option<PathBuf>,
    pub loglevel: String,
    #[serde(deserialize_with = "deserialize_quota")]
    pub default_quota_bytes: i64,
    pub session_days: i64,
    pub upload_expiry_days: i64,
    pub reconcile_seconds: u64,
    pub access_log_days: i64,
}

fn deserialize_quota<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    struct QuotaVisitor;
    impl serde::de::Visitor<'_> for QuotaVisitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an integer byte count or a size string such as \"1.5G\"")
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<i64, E> {
            self.visit_str(&value.to_string())
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<i64, E> {
            self.visit_str(&value.to_string())
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<i64, E> {
            crate::size::parse_size(value).map_err(E::custom)
        }
    }
    deserializer.deserialize_any(QuotaVisitor)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            webroot: "http://localhost:8080".into(),
            listen: "127.0.0.1:8080".parse().expect("constant address"),
            data_dir: "data".into(),
            static_dir: "res/static".into(),
            imprint: None,
            loglevel: "info".into(),
            default_quota_bytes: 100 * 1024 * 1024 * 1024,
            session_days: 30,
            upload_expiry_days: 30,
            reconcile_seconds: 60,
            access_log_days: 90,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut value: Self = config::Config::builder()
            .add_source(config::File::from(path))
            .build()
            .context("read configuration (copy config.json.example to config.json first)")?
            .try_deserialize()
            .context("invalid configuration")?;
        value.validate()?;
        let base = path.parent().unwrap_or(Path::new("."));
        if value.data_dir.is_relative() {
            value.data_dir = base.join(&value.data_dir);
        }
        if value.static_dir.is_relative() {
            value.static_dir = base.join(&value.static_dir);
        }
        if let Some(imprint) = &mut value.imprint {
            if imprint.is_relative() {
                *imprint = base.join(&*imprint);
            }
        }
        Ok(value)
    }

    pub fn validate(&mut self) -> anyhow::Result<()> {
        let url = url::Url::parse(&self.webroot).context("webroot must be an absolute URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("webroot must be an http(s) URL without credentials, query or fragment");
        }
        if !url
            .path()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-".contains(&b))
            || url.path().contains("//")
        {
            bail!("webroot path may contain only ASCII letters, numbers, /, _ and -");
        }
        self.webroot = url.as_str().trim_end_matches('/').to_owned();
        if self.default_quota_bytes < 0
            || self.default_quota_bytes > 9_007_199_254_740_991
            || !(1..=3650).contains(&self.session_days)
            || !(1..=3650).contains(&self.upload_expiry_days)
            || !(1..=86400).contains(&self.reconcile_seconds)
            || !(1..=3650).contains(&self.access_log_days)
        {
            bail!("invalid quota, retention or reconciliation interval");
        }
        Ok(())
    }
    pub fn prefix(&self) -> &str {
        let start = self.webroot.find("://").expect("validated URL") + 3;
        self.webroot[start..]
            .find('/')
            .map_or("", |i| &self.webroot[start + i..])
    }
    pub fn origin(&self) -> String {
        url::Url::parse(&self.webroot)
            .expect("validated URL")
            .origin()
            .ascii_serialization()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> anyhow::Result<Settings> {
        let mut settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(json, config::FileFormat::Json))
            .build()?
            .try_deserialize()?;
        settings.validate()?;
        Ok(settings)
    }

    #[test]
    fn imprint_is_optional() {
        assert!(read("{}").unwrap().imprint.is_none());
        assert!(read(r#"{"imprint":null}"#).unwrap().imprint.is_none());
        assert_eq!(
            read(r#"{"imprint":"imprint.html"}"#).unwrap().imprint,
            Some(PathBuf::from("imprint.html"))
        );
    }

    #[test]
    fn quota_accepts_numbers_and_cli_size_strings() {
        for (value, expected) in [
            ("107374182400", 107_374_182_400),
            ("\"100G\"", 107_374_182_400),
            ("\"1.5G\"", 1_610_612_736),
            ("\"20m\"", 20_971_520),
            ("\"3KiB\"", 3072),
            ("\"107374182400\"", 107_374_182_400),
            ("0", 0),
            ("9007199254740991", 9_007_199_254_740_991),
        ] {
            assert_eq!(
                read(&format!("{{\"default_quota_bytes\":{value}}}"))
                    .unwrap()
                    .default_quota_bytes,
                expected,
                "{value}"
            );
        }
        assert_eq!(
            read("{}").unwrap().default_quota_bytes,
            Settings::default().default_quota_bytes
        );
    }

    #[test]
    fn quota_rejects_invalid_types_sizes_and_overflow() {
        for value in [
            "-1",
            "1.5",
            "true",
            "null",
            "[]",
            "{}",
            "\"bad\"",
            "\"-1G\"",
            "\"8P\"",
            "9007199254740992",
        ] {
            assert!(
                read(&format!("{{\"default_quota_bytes\":{value}}}")).is_err(),
                "{value}"
            );
        }
    }
}
