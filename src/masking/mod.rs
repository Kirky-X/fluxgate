// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 出站请求掩码（outbound-masking，feature `masking`）。
//!
//! 发送上游前对最终出站 JSON 的字符串值做规则/熵检测，命中值替换为
//! 确定性占位符 `{{FLUX_<KIND>_<8HEX>}}`（哈希掺 pepper——三态来源
//! env/0600 文件/进程内随机，见 `placeholder` 模块文档；同 pepper 下
//! 跨进程/多副本一致，pepper 各异时上游无法离线预计算或跨实例关联），
//! 回程（整量/流式）只还原**本请求登记过**的占位符——厂商侧永不落真值，
//! 伪造/他请求回显的占位符原样透传。反向映射仅存内存（LRU + TTL），
//! 任何落盘路径不得包含原值。fail-closed：掩码/还原任一步 Err 或 panic
//! → 请求 503 拒发，绝不原文出站。
//!
//! 本文件只承载类型与 trait（接口隔离）；方法实现与子模块见同目录文件。

use std::ops::Range;

pub mod gates;
pub mod kinds;
mod ledger;
mod masker;
mod placeholder;
mod restorer;
mod rule_detector;
mod stream_restore;

pub use gates::MaskGate;
pub use ledger::MaskingLedger;
pub use masker::{mask_json, ActionMap, AllowList, KindSet, MaskedHitInfo};
pub use placeholder::{placeholder_for, MaskTicket, PlaceholderRegistry};
pub use restorer::restore_json;
pub use rule_detector::RuleDetector;
pub use stream_restore::StreamRestorer;

use thiserror::Error;

/// 敏感值种类（占位符 kind 段与 `actions`/`kinds` 配置的合法值集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaskKind {
    /// 供应商 API 密钥形态（`sk-`、`ghp_`、`xoxb-`、`AKIA…` 等）
    ApiKey,
    /// PEM 私钥块
    PrivateKey,
    /// JWT / 高熵 token 等令牌形态
    Token,
    /// 电子邮件地址
    Email,
    /// 卡号形态（自定义规则映射）
    Card,
    /// 证件/身份号形态（自定义规则映射）
    Id,
    /// 自定义词表与正则命中
    Custom,
}

/// 单次检测命中：种类 + 原文字节区间 + 检测来源标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskHit {
    pub kind: MaskKind,
    pub span: Range<usize>,
    /// 检测来源（台账 finder 面）：inklog pattern 名（如 `openai_sk`）、
    /// 自定义正则注册名（`custom_rule_N`）或词表命中（`word`）。
    pub finder: String,
}

/// 掩码动作（`[masking].actions` 按 kind 配置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskAction {
    /// 替换为占位符
    Mask,
    /// 只记指标不改写
    Warn,
    /// 整请求拒绝（400 + `masking-blocked`）
    Block,
}

/// 掩码错误（fail-closed 语义：[`MaskError::Unavailable`] 一律映射 503 拒发，
/// 绝不原文出站）。
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum MaskError {
    /// 检测/掩码/还原任一步 Err 或 panic：请求拒绝（503 `masking-unavailable`）
    #[error("masking unavailable: request refused (fail-closed)")]
    Unavailable,
    /// `Block` 动作命中：整请求拒绝（400 `masking-blocked`）
    #[error("masking blocked: {kind:?}")]
    Blocked {
        /// 触发拦截的种类
        kind: MaskKind,
    },
    /// 自定义正则非法（构造期拒绝）
    #[error("invalid masking rule: {detail}")]
    InvalidRule {
        /// 编译失败原因
        detail: String,
    },
}

/// 检测器接口：报告 `text` 中的敏感值命中（种类 + 字节区间）。
///
/// 实现必须纯读（不改写、不计数副作用归实现自管）；返回区间按字节计，
/// 调用方保证不越界重组。
pub trait MaskDetector: Send + Sync {
    fn detect(&self, text: &str) -> Vec<MaskHit>;
}
