// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! [`MaskKind`](super::MaskKind) 的方法实现（规则14：mod.rs 只承载类型，
//! 实现拆本文件）。

use super::MaskKind;

impl MaskKind {
    /// 配置串闭集解析（`[masking].kinds` / `actions` 键的合法值集）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "api_key" => Some(Self::ApiKey),
            "private_key" => Some(Self::PrivateKey),
            "token" => Some(Self::Token),
            "email" => Some(Self::Email),
            "card" => Some(Self::Card),
            "id" => Some(Self::Id),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// 占位符 kind 段大写标签（`{{FLUX_<TAG>_<8HEX>}}`）。
    pub fn as_tag(self) -> &'static str {
        match self {
            Self::ApiKey => "APIKEY",
            Self::PrivateKey => "PRIVATEKEY",
            Self::Token => "TOKEN",
            Self::Email => "EMAIL",
            Self::Card => "CARD",
            Self::Id => "ID",
            Self::Custom => "CUSTOM",
        }
    }

    /// 配置串形态（与 [`Self::parse`] 互逆）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::PrivateKey => "private_key",
            Self::Token => "token",
            Self::Email => "email",
            Self::Card => "card",
            Self::Id => "id",
            Self::Custom => "custom",
        }
    }

    /// 全种类集（`kinds` 缺省 = 全启用）。
    pub fn all() -> [MaskKind; 7] {
        [
            Self::ApiKey,
            Self::PrivateKey,
            Self::Token,
            Self::Email,
            Self::Card,
            Self::Id,
            Self::Custom,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_as_str_roundtrip() {
        for kind in MaskKind::all() {
            assert_eq!(MaskKind::parse(kind.as_str()), Some(kind), "{kind:?}");
        }
        assert_eq!(MaskKind::parse("unknown"), None);
        assert_eq!(MaskKind::parse(""), None);
    }

    #[test]
    fn tags_are_unique_uppercase() {
        let mut tags: Vec<&'static str> = MaskKind::all().iter().map(|k| k.as_tag()).collect();
        let len = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), len, "kind 标签不得撞名");
        assert!(tags
            .iter()
            .all(|t| !t.is_empty() && t.chars().all(char::is_uppercase)));
    }
}
