// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 占位符定位与还原原语：完整占位符（`{{FLUX_<KIND>_<8HEX>}}`，8 位大写
//! hex）的无正则定位（位置级模式匹配，hex 位接受任意 hex 字符）、候选前缀
//! 判定（流式 hold 判据）与字符串级还原（注册表命中替换，未注册原样透传
//! ——上游回显的伪造占位符不丢失、不误还原）。

use super::{MaskKind, MaskTicket, PlaceholderRegistry};
use std::ops::Range;

/// 单 kind 的完整占位符字节模式（hex 位以字面占位，匹配时作通配）。
fn pattern_for(kind: MaskKind) -> Vec<u8> {
    let mut p = super::placeholder::PLACEHOLDER_PREFIX.as_bytes().to_vec();
    p.extend_from_slice(kind.as_tag().as_bytes());
    p.push(b'_');
    p.extend_from_slice(b"00000000");
    p.extend_from_slice(super::placeholder::PLACEHOLDER_SUFFIX.as_bytes());
    p
}

/// hex 通配区起点：`{{FLUX_` + kind 标签 + `_` 之后。
fn hex_region(kind: MaskKind) -> (usize, usize) {
    let start = super::placeholder::PLACEHOLDER_PREFIX.len() + kind.as_tag().len() + 1;
    (start, start + 8)
}

/// `s` 是否可能是某占位符的前缀（位置级匹配；调用方保证 `s` 以 `{` 开头
/// 才有意义——首字节即失配则恒 false）。
pub(crate) fn is_placeholder_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b[0] != b'{' {
        return false;
    }
    for kind in MaskKind::all() {
        let p = pattern_for(kind);
        if b.len() > p.len() {
            continue;
        }
        let (hex_start, hex_end) = hex_region(kind);
        let ok = b.iter().enumerate().all(|(i, c)| {
            if (hex_start..hex_end).contains(&i) {
                c.is_ascii_hexdigit()
            } else {
                *c == p[i]
            }
        });
        if ok {
            return true;
        }
    }
    false
}

/// 从 `from` 起定位首个完整占位符，返回（字节区间，占位符串）。
pub(crate) fn find_complete(text: &str, from: usize) -> Option<(Range<usize>, String)> {
    let b = text.as_bytes();
    let mut i = from;
    while i < b.len() {
        if b[i] == b'{' {
            for kind in MaskKind::all() {
                let p = pattern_for(kind);
                let end = i + p.len();
                if end > b.len() {
                    continue;
                }
                let (hex_start, hex_end) = hex_region(kind);
                let ok = b[i..end].iter().enumerate().all(|(j, c)| {
                    if (hex_start..hex_end).contains(&j) {
                        c.is_ascii_hexdigit()
                    } else {
                        *c == p[j]
                    }
                });
                if ok {
                    return Some((i..end, text[i..end].to_string()));
                }
            }
        }
        i += 1;
    }
    None
}

/// 带计数的字符串级还原（返回还原串与注册命中数——语义为「回填次数」，
/// 未注册占位符不计）。放行判据：`ticket` 为 Some 时只还原本请求登记过的
/// 占位符（跨请求/伪造形态原样透传）；None = 不限域（注册表全量，测试/SDK 面）。
pub(crate) fn restore_in_string_counted(
    text: &str,
    registry: &PlaceholderRegistry,
    ticket: Option<&MaskTicket>,
) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut restored = 0usize;
    let mut pos = 0usize;
    while let Some((span, placeholder)) = find_complete(text, pos) {
        out.push_str(&text[pos..span.start]);
        let in_scope = ticket.is_none_or(|t| t.contains(&placeholder));
        let value = if in_scope {
            registry.restore(&placeholder)
        } else {
            None
        };
        match value {
            Some(value) => {
                out.push_str(&value);
                restored += 1;
            }
            None => out.push_str(&placeholder),
        }
        pos = span.end;
    }
    out.push_str(&text[pos..]);
    (out, restored)
}

/// 非流式整量还原：对响应 Value 的全部字符串值递归回填占位符。返回还原
/// 总数（域外/未注册占位符不计、不报错——上游编造的形态原样保留）。
pub fn restore_json(
    body: &mut serde_json::Value,
    registry: &PlaceholderRegistry,
    ticket: Option<&MaskTicket>,
) -> usize {
    match body {
        serde_json::Value::String(s) => {
            let (restored, count) = restore_in_string_counted(s, registry, ticket);
            *s = restored;
            count
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|item| restore_json(item, registry, ticket))
            .sum(),
        serde_json::Value::Object(map) => map
            .values_mut()
            .map(|v| restore_json(v, registry, ticket))
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matcher_shapes() {
        assert!(is_placeholder_prefix("{{"));
        assert!(is_placeholder_prefix("{{FLUX_"));
        assert!(is_placeholder_prefix("{{FLUX_APIKEY_"));
        assert!(is_placeholder_prefix("{{FLUX_APIKEY_9F"));
        assert!(is_placeholder_prefix("{{FLUX_PRIVATEKEY_01234567}"));
        assert!(!is_placeholder_prefix("{x"));
        assert!(!is_placeholder_prefix("{{FLUX_LOW_9F"));
        assert!(!is_placeholder_prefix("{{FLUX_APIKEY_GG"));
        // 单闭括号可延续为 }}——仍是候选；完整形态是自身前缀
        assert!(is_placeholder_prefix("{{FLUX_APIKEY_9F3C21D7}"));
        assert!(is_placeholder_prefix("{{FLUX_APIKEY_9F3C21D7}}"));
        // 第 9 位出现 hex（} 位被占）→ 不可能延续
        assert!(!is_placeholder_prefix("{{FLUX_APIKEY_9F3C21D71"));
    }

    #[test]
    fn complete_finder_exact_span() {
        let text = "a {{FLUX_TOKEN_00112233}} b";
        let (span, ph) = find_complete(text, 0).expect("hit");
        assert_eq!(&text[span.clone()], "{{FLUX_TOKEN_00112233}}");
        assert_eq!(ph, "{{FLUX_TOKEN_00112233}}");
        assert!(
            find_complete("{{FLUX_TOKEN_0011223}}", 0).is_none(),
            "7 位 hex 不完整"
        );
    }
}
