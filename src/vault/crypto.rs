// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Fluxgate Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! cocoon AEAD 封装：逐条 blob 加解密、掩码、指纹与条目 id 生成。
//!
//! master key 本身是随机 32 字节，`MiniCocoon::from_key` 跳过 PBKDF2；
//! `KDF_SEED` 仅作域分隔常量，不承担口令派生职责。

use cocoon::MiniCocoon;
use rand::RngExt;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::i18n::t_args;
use crate::vault::VaultError;

/// 32 字节 master key 的长度约定（hex 编码后 64 字符）。
pub(crate) const MASTER_KEY_BYTES: usize = 32;

/// 域分隔 seed：固定常量，非口令 KDF 盐。cocoon `from_key` 要求恰好 32 字节，
/// 长度不符运行时 panic——常量文本必须维持 32 字节（改名等文本编辑不得缩短）。
const KDF_SEED: &[u8] = b"fluxgate.vault.kdf.seed.v1.00000";

/// fingerprint / id 共用的 hex 截断长度。
const FINGERPRINT_LEN: usize = 16;

pub(crate) fn cipher(master_key: &[u8]) -> MiniCocoon {
    MiniCocoon::from_key(master_key, KDF_SEED)
}

/// 明文 → self-contained 密文 blob（36 字节前缀：nonce 12 + 长度 8 + tag 16）。
pub(crate) fn seal(master_key: &[u8], plaintext: &SecretString) -> Result<Vec<u8>, VaultError> {
    cipher(master_key)
        .wrap(plaintext.expose_secret().as_bytes())
        .map_err(|e| {
            VaultError::Crypto(t_args(
                "vault-seal-blob-failed",
                &[("error", e.to_string())],
            ))
        })
}

/// 密文 blob → 明文（SecretString 包装，drop 即擦除）。
pub(crate) fn open(master_key: &[u8], blob: &[u8]) -> Result<SecretString, VaultError> {
    let bytes = cipher(master_key).unwrap(blob).map_err(|e| {
        VaultError::Crypto(t_args(
            "vault-open-blob-failed",
            &[("error", e.to_string())],
        ))
    })?;
    let text = String::from_utf8(bytes).map_err(|e| {
        VaultError::Crypto(t_args(
            "vault-decrypted-not-utf8",
            &[("error", e.to_string())],
        ))
    })?;
    Ok(SecretString::new(text.into()))
}

/// 掩码：len > 8 → 前 3 + `***` + 后 4；否则恒定 `***`。
pub(crate) fn mask_key(plaintext: &str) -> String {
    let chars: Vec<char> = plaintext.chars().collect();
    if chars.len() > 8 {
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}***{tail}")
    } else {
        "***".to_string()
    }
}

/// 明文 SHA256 hex 前 16 字符；(vendor_id, fingerprint) 唯一约束的指纹面。
pub(crate) fn fingerprint(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    hex::encode(&digest[..FINGERPRINT_LEN / 2])
}

/// unix epoch 毫秒时间戳。
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 条目 id：16 字节随机 hex（不引 uuid 依赖）。
pub(crate) fn new_id() -> String {
    hex::encode(rand::rng().random::<[u8; 16]>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> Vec<u8> {
        vec![7u8; MASTER_KEY_BYTES]
    }

    /// cocoon 对 seed 长度不符是运行时 panic（非编译错误）：任何文本编辑
    /// 缩短 KDF_SEED 都会先在此红。
    #[test]
    fn kdf_seed_is_32_bytes() {
        assert_eq!(KDF_SEED.len(), MASTER_KEY_BYTES);
    }

    #[test]
    fn seal_open_roundtrip_preserves_plaintext() {
        let key = test_key();
        let secret = SecretString::new("sk-test-abcdef123456".to_string().into());
        let blob = seal(&key, &secret).expect("seal");
        let revealed = open(&key, &blob).expect("open");
        assert_eq!(revealed.expose_secret(), "sk-test-abcdef123456");
    }

    #[test]
    fn tampered_blob_fails_to_open() {
        let key = test_key();
        let secret = SecretString::new("sk-test-abcdef123456".to_string().into());
        let mut blob = seal(&key, &secret).expect("seal");
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(matches!(open(&key, &blob), Err(VaultError::Crypto(_))));
    }

    #[test]
    fn wrong_master_key_fails_to_open() {
        let secret = SecretString::new("sk-test-abcdef123456".to_string().into());
        let blob = seal(&test_key(), &secret).expect("seal");
        let other = vec![9u8; MASTER_KEY_BYTES];
        assert!(matches!(open(&other, &blob), Err(VaultError::Crypto(_))));
    }

    #[test]
    fn mask_boundary_len8_is_constant_and_len9_reveals_ends() {
        assert_eq!(mask_key("12345678"), "***");
        assert_eq!(mask_key("123456789"), "123***6789");
        assert_eq!(mask_key("short"), "***");
    }

    #[test]
    fn fingerprint_is_stable_and_truncated() {
        assert_eq!(fingerprint("sk-abc"), fingerprint("sk-abc"));
        assert_ne!(fingerprint("sk-abc"), fingerprint("sk-abd"));
        assert_eq!(fingerprint("sk-abc").len(), FINGERPRINT_LEN);
    }

    #[test]
    fn new_id_is_32_hex_chars_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
