// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 词法压缩（N2，training-free 确定性路线）：对超长消息做无损语义风险
//! 最低的词法剪枝——压缩连续空白、折叠连续重复行、丢弃纯标点行。
//! 规则全部确定性（规则7），结果长度受 `ratio_ceiling` 下限保护。
//! 仅 `X-Compress: 1` 显式触发；system 与其余消息永不触碰。

/// 对单条消息内容做确定性词法剪枝。
/// 返回 None = 不压缩（长度未达 min_chars，或无可剪空间）。
/// 下限保护：结果字符数 ≥ 原文 ×(1 − ratio_ceiling)，达不到就少剪。
pub fn compress(content: &str, min_chars: usize, ratio_ceiling: f64) -> Option<String> {
    if content.chars().count() <= min_chars {
        return None;
    }
    let total = content.chars().count();
    let floor = ((total as f64) * (1.0 - ratio_ceiling.clamp(0.0, 1.0))) as usize;
    let mut out_lines: Vec<String> = Vec::new();
    let mut out_chars = 0usize;
    let mut prev_line: Option<String> = None;
    let mut removed = 0usize;

    for line in content.lines() {
        let trimmed_end = line.trim_end();
        // 1) 纯标点/空白行：可剪
        let is_pure_punct = !trimmed_end.is_empty()
            && trimmed_end
                .chars()
                .all(|c| !c.is_alphanumeric() && !is_cjk(c));
        // 2) 连续重复行：保留首行，后续可剪
        let dup = prev_line.as_deref() == Some(trimmed_end);
        // +2 = 换行 + join 余量（kept 行间重加的 \n 会计差，防 floor 边界差一字整体放弃）
        let would_remove = trimmed_end.chars().count() + 2;
        let removable = (is_pure_punct || dup || trimmed_end.is_empty())
            && removed + would_remove <= total.saturating_sub(floor);

        if removable {
            removed += would_remove;
            prev_line = Some(trimmed_end.to_string());
            continue;
        }
        // 3) 行内空白归一（连续空格/制表 → 单空格；保留行结构）
        let mut normalized = String::with_capacity(line.len());
        let mut prev_space = false;
        for c in line.chars() {
            if c == ' ' || c == '\t' {
                if !prev_space {
                    normalized.push(' ');
                }
                prev_space = true;
            } else {
                normalized.push(c);
                prev_space = false;
            }
        }
        out_chars += normalized.chars().count() + 1;
        prev_line = Some(normalized.clone());
        out_lines.push(normalized);
    }

    let out = out_lines.join("\n");
    if out.chars().count() > total {
        return None;
    }
    if out.chars().count() < floor {
        // 下限保护触发：剪枝过度（如空白归一占比过高）→ 放弃压缩保留原文，
        // 禁止截断原文尾部真实内容
        return None;
    }
    if out.chars().count() >= total {
        return None;
    }
    let _ = out_chars;
    Some(out)
}

/// handler 接线入口：enabled + `X-Compress: 1` 时压缩最长非 system 消息。
/// 返回是否发生压缩（响应头 `X-Compressed: 1` 的来源）。body 原地改写，
/// 必须在难度特征/缓存键/前缀哈希计算**之前**调用，保证下游口径一致。
pub fn apply_to_body(
    config: &crate::config::ConfigManager,
    headers: &axum::http::HeaderMap,
    body: &mut serde_json::Value,
) -> bool {
    let (enabled, min_chars, ratio_ceiling) = {
        let c = config.read();
        let l = &c.lexcompress;
        (l.enabled, l.min_chars, l.ratio_ceiling)
    };
    if !enabled {
        return false;
    }
    let requested = headers
        .get("x-compress")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if !requested {
        return false;
    }
    let Some(messages) = body
        .as_object_mut()
        .and_then(|o| o.get_mut("messages"))
        .and_then(|m| m.as_array_mut())
    else {
        return false;
    };
    // 最长的非 system 消息（body 数组原地改写）
    let mut target: Option<(usize, usize)> = None; // (idx, char_len)
    for (idx, msg) in messages.iter().enumerate() {
        let is_system = msg.get("role").and_then(|r| r.as_str()) == Some("system");
        let len = msg
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        if !is_system && len > min_chars && target.map(|(_, l)| len > l).unwrap_or(true) {
            target = Some((idx, len));
        }
    }
    let Some((idx, _)) = target else {
        return false;
    };
    let Some(content) = messages[idx]
        .get("content")
        .and_then(|c| c.as_str())
        .map(str::to_string)
    else {
        return false;
    };
    let Some(compressed) = compress(&content, min_chars, ratio_ceiling) else {
        return false;
    };
    if let Some(obj) = messages[idx].as_object_mut() {
        obj.insert("content".to_string(), serde_json::Value::String(compressed));
    }
    true
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_not_compressed() {
        assert_eq!(compress("短文本", 4000, 0.5), None);
    }

    #[test]
    fn repeated_lines_collapsed_with_floor() {
        let line = "这是一行重复的日志内容啊\n";
        let content = line.repeat(2000); // ~24KB
        let out = compress(&content, 4000, 0.5).expect("可压缩");
        let out_len = out.chars().count();
        assert!(
            out_len >= (content.chars().count() as f64 * 0.5) as usize,
            "下限保护: {out_len}"
        );
        assert!(out_len < content.chars().count(), "必须实际变短");
    }

    #[test]
    fn whitespace_normalized_and_cjk_preserved() {
        let para = "中文    内容\t\t带很多空白。".repeat(500);
        let content = format!("{}\n\n\n{}", para, para);
        let out = compress(&content, 4000, 0.5).expect("可压缩");
        assert!(!out.contains("  "), "连续空格应归一");
        assert!(out.contains('中'), "CJK 保留");
    }

    #[test]
    fn pure_punctuation_lines_dropped() {
        // 正文垫足 4000+，标点分隔线仅占小头——floor = (1-0.9)·total 不会
        // 强行保留标点（剪除预算 = total − floor ≥ 标点总量）
        let prose = "正文开头的实际内容。".repeat(400);
        let sep = "-=-=-=\n".repeat(100);
        let content = format!("{prose}\n{sep}结尾也是正文。");
        let out = compress(&content, 4000, 0.9).expect("可压缩");
        assert!(out.contains("正文开头"), "正文保留");
        assert!(!out.contains("=-=-="), "纯标点行剪除");
    }
}
