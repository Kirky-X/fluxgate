// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 流式回程还原：SSE 逐事件文本增量经 [`StreamRestorer`] 回填占位符。
//!
//! 跨 chunk 边界用**尾部保持缓冲**：无法排除是占位符前缀的尾段（以 `{`
//! 开头且位置级可延续为完整形态）滞留到下一增量或 flush；其余立即可发
//! ——纯文本零滞留（无 `{` 的增量整段透传），候选区受占位符最大长度
//! （28 字节）约束，远低于 96 字节的零延迟语义上界。
//!
//! 实例语义：每个文本通道（content delta）与每个 tool_call 增量流独占
//! 一个实例——arguments 是未转义纯文本增量（IR 层），与内容通道同语义，
//! 互不串扰。

use super::{restorer, MaskTicket, PlaceholderRegistry};
use std::sync::Arc;

/// 流式还原器：`feed` 增量、`flush` 收流。非 `Clone`（缓冲状态独占）。
pub struct StreamRestorer {
    registry: Arc<PlaceholderRegistry>,
    /// 还原放行域（Some = 只认本请求登记的占位符，生产路径；None = 注册表
    /// 全量，测试/SDK 面）。
    ticket: Option<Arc<MaskTicket>>,
    buf: String,
    /// 累计回填占位符数（观测面；门层差值打点）
    restored: u64,
}

impl std::fmt::Debug for StreamRestorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamRestorer")
            .field("buffered_len", &self.buf.len())
            .finish_non_exhaustive()
    }
}

impl StreamRestorer {
    /// 绑定注册表的还原器（每通道一个实例）；不限域。
    pub fn new(registry: Arc<PlaceholderRegistry>) -> Self {
        Self {
            registry,
            ticket: None,
            buf: String::new(),
            restored: 0,
        }
    }

    /// 请求域绑定：只还原 `ticket` 登记过的占位符（生产路径——伪造/
    /// 他请求回显的占位符原样透传）。
    pub fn scoped(registry: Arc<PlaceholderRegistry>, ticket: Arc<MaskTicket>) -> Self {
        Self {
            registry,
            ticket: Some(ticket),
            buf: String::new(),
            restored: 0,
        }
    }

    /// 输入一个增量，返回立即可发的还原文本。
    pub fn feed(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        self.drain(false)
    }

    /// 收流：还原缓冲内全部完整占位符并清空缓冲（悬空候选原样下发）。
    pub fn flush(&mut self) -> String {
        self.drain(true)
    }

    /// 当前滞留字节数（候选前缀；≤ [`PLACEHOLDER_MAX_LEN`]）。
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// 累计回填占位符数（注册命中；未注册形态不计）。
    pub fn restored_total(&self) -> u64 {
        self.restored
    }

    /// 还原缓冲内完整占位符后，按 hold 点切分输出/滞留。
    fn drain(&mut self, final_flush: bool) -> String {
        let (restored, hits) =
            restorer::restore_in_string_counted(&self.buf, &self.registry, self.ticket.as_deref());
        self.restored += hits as u64;
        let hold = if final_flush {
            restored.len()
        } else {
            hold_point(&restored)
        };
        let out = restored[..hold].to_string();
        self.buf = restored[hold..].to_string();
        out
    }
}

/// hold 点：最早一处「可能是占位符前缀」的位置（候选区必然以 `{` 起始，
/// 字符边界安全）；无候选则全部可发。
fn hold_point(s: &str) -> usize {
    let bytes = s.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'{' && restorer::is_placeholder_prefix(&s[i..]) {
            return i;
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::masking::placeholder::PLACEHOLDER_MAX_LEN;
    use crate::masking::MaskKind;

    fn reg() -> Arc<PlaceholderRegistry> {
        Arc::new(PlaceholderRegistry::new())
    }

    #[test]
    fn split_placeholder_recovers_via_hold() {
        let registry = reg();
        let secret = "sk-unit-secret";
        let ph = registry.mask(MaskKind::ApiKey, secret);
        let mut r = StreamRestorer::new(Arc::clone(&registry));
        let (a, b) = ph.split_at(10);
        let out = format!("{}{}{}", r.feed(a), r.feed(b), r.flush());
        assert_eq!(out, secret);
    }

    #[test]
    fn hold_capped_at_placeholder_max_len() {
        let mut r = StreamRestorer::new(reg());
        let _ = r.feed("tail{{FLUX_PRIVATEKEY_ABCDEF01");
        assert!(
            r.buffered_len() <= PLACEHOLDER_MAX_LEN,
            "{}",
            r.buffered_len()
        );
        assert_eq!(r.buffered_len(), "tail{{FLUX_PRIVATEKEY_ABCDEF01".len() - 4);
    }

    #[test]
    fn no_brace_text_never_buffers() {
        let mut r = StreamRestorer::new(reg());
        assert_eq!(r.feed("abc def"), "abc def");
        assert_eq!(r.buffered_len(), 0);
    }
}
