// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! master key 分层解析：env `FLUXGATE_MASTER_KEY` 优先，缺失时自动生成
//! `~/.fluxgate/vault_master.key`（0600）。master key 丢失 = 柜内密钥全部
//! 不可恢复（无找回路径，设计使然）；部署方负责文件备份。

use rand::RngExt;

use crate::i18n::{t, t_args};
use crate::vault::crypto::MASTER_KEY_BYTES;
use crate::vault::VaultError;

/// 主钥环境变量名。
pub const MASTER_KEY_ENV: &str = "FLUXGATE_MASTER_KEY";

/// hex 编码后 64 字符（32 字节）。
const MASTER_KEY_HEX_LEN: usize = MASTER_KEY_BYTES * 2;

/// 生产入口：读 env 与默认 home 路径后走分层解析。
pub fn load_or_create_master_key() -> Result<Vec<u8>, VaultError> {
    let env_value = std::env::var(MASTER_KEY_ENV).ok().filter(|s| !s.is_empty());
    let path = home_master_key_path()?;
    load_or_create_at(env_value.as_deref(), &path)
}

/// 默认主钥文件：`~/.fluxgate/vault_master.key`。
pub fn home_master_key_path() -> Result<std::path::PathBuf, VaultError> {
    let home =
        std::env::home_dir().ok_or_else(|| VaultError::MasterKey(t("vault-home-not-found")))?;
    Ok(home.join(".fluxgate").join("vault_master.key"))
}

/// 分层解析核心（路径与 env 值注入，便于测试隔离）：
/// 1. env 存在 → 严格 64 字符 hex 解析，非法即报错，不回退文件；
/// 2. env 缺失 → 读文件；文件不存在 → rand 生成并写盘（Unix 0600）。
pub fn load_or_create_at(
    env_value: Option<&str>,
    file_path: &std::path::Path,
) -> Result<Vec<u8>, VaultError> {
    if let Some(value) = env_value {
        return parse_hex_key(value);
    }
    if let Ok(encoded) = std::fs::read_to_string(file_path) {
        return parse_hex_key(encoded.trim());
    }
    let key = rand::rng().random::<[u8; MASTER_KEY_BYTES]>();
    write_key_file(file_path, &key)?;
    Ok(key.to_vec())
}

fn parse_hex_key(value: &str) -> Result<Vec<u8>, VaultError> {
    let trimmed = value.trim();
    if trimmed.len() != MASTER_KEY_HEX_LEN || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(VaultError::MasterKey(t_args(
            "vault-master-key-hex-len",
            &[
                ("env", MASTER_KEY_ENV.to_string()),
                ("expected", MASTER_KEY_HEX_LEN.to_string()),
                ("actual", trimmed.len().to_string()),
            ],
        )));
    }
    hex::decode(trimmed).map_err(|e| {
        VaultError::MasterKey(t_args(
            "vault-master-key-hex-decode-failed",
            &[("error", e.to_string())],
        ))
    })
}

fn write_key_file(path: &std::path::Path, key: &[u8]) -> Result<(), VaultError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            VaultError::MasterKey(t_args(
                "vault-master-key-create-dir-failed",
                &[("error", e.to_string())],
            ))
        })?;
    }
    std::fs::write(path, hex::encode(key)).map_err(|e| {
        VaultError::MasterKey(t_args(
            "vault-master-key-write-failed",
            &[("error", e.to_string())],
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            VaultError::MasterKey(t_args(
                "vault-master-key-restrict-failed",
                &[("error", e.to_string())],
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_HEX: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    #[test]
    fn env_value_takes_priority_over_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("master.key");
        std::fs::write(&path, GOOD_HEX).expect("seed file");
        let key = load_or_create_at(Some(GOOD_HEX), &path).expect("key from env");
        assert_eq!(key, hex::decode(GOOD_HEX).expect("decode"));
    }

    #[test]
    fn invalid_env_value_fails_without_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("master.key");
        std::fs::write(&path, GOOD_HEX).expect("seed file");
        let err = load_or_create_at(Some("zz-not-hex"), &path);
        assert!(matches!(err, Err(VaultError::MasterKey(_))));
        // 文件在，但绝不允许静默回退
        assert!(path.exists());
    }

    #[test]
    fn generated_file_is_persistent_and_restricted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("master.key");
        let first = load_or_create_at(None, &path).expect("generate");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let second = load_or_create_at(None, &path).expect("reload");
        assert_eq!(first, second);
    }
}
