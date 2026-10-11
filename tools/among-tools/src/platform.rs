use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub version: String,
    pub architecture: String,
}

pub fn hosted_rehearsal(approved: bool, actions: Option<&str>, runner: Option<&str>) -> bool {
    approved && actions == Some("true") && runner == Some("github-hosted")
}

pub fn parse(os_release: &str, architecture: &str) -> Result<Platform> {
    let mut values = BTreeMap::new();
    for line in os_release
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line.split_once('=').ok_or("malformed os-release record")?;
        if values
            .insert(key, value.trim_matches('"').trim_matches('\''))
            .is_some()
        {
            return Err("duplicate os-release field".into());
        }
    }
    let architecture = match architecture {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        _ => return Err("unsupported native architecture".into()),
    };
    Ok(Platform {
        os: values.get("ID").ok_or("os-release lacks ID")?.to_string(),
        version: values
            .get("VERSION_ID")
            .ok_or("os-release lacks VERSION_ID")?
            .to_string(),
        architecture: architecture.into(),
    })
}

pub fn discover() -> Result<Platform> {
    parse(
        &fs::read_to_string("/etc/os-release")?,
        std::env::consts::ARCH,
    )
}

pub fn validate_target(
    actual: &Platform,
    os: &str,
    version: &str,
    architecture: &str,
) -> Result<()> {
    if !matches!((os, version), ("ubuntu", "24.04") | ("debian", "13"))
        || !matches!(architecture, "amd64" | "arm64")
    {
        return Err("unsupported distribution target".into());
    }
    if actual.os != os || actual.version != version || actual.architecture != architecture {
        return Err("discovered platform differs from declared target".into());
    }
    Ok(())
}
