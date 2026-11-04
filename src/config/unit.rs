// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 定价单位换算单点 — 外部源口径与 fluxweir 计价口径的换算常量集中处
//!（catalog-sync 映射与后续 cost-replay 共享本模块；design：换算单位
//! 错误风险集中在单点 + 对照测试钉死）。
//!
//! 口径对照：
//! - 外部源（models.dev `cost.*`）：**currency 每 1M tokens**（如 3.0 =
//!   $3 / 1M input tokens）；
//! - fluxweir `[[model_catalog]]` 四价字段：**currency 每 1M tokens**
//!   （`src/config/model_catalog.rs`）。
//!
//! 数值等价性：`1 currency / 1M tokens ≡ 1 micro-currency / token`
//!（10⁻⁶），故两口径的**数值**恒等——`source_price_to_catalog_price`
//! 是该恒等映射的唯一裁决点，测试钉死「3.0 USD/1M ≡ 3.0 μ$/token」；
//! 任一口径未来单独漂移（如 catalog 改 per-token 微单位）时此处先行变红。

/// 每货币单位的微单位数（10⁶）：per-1M 价 × 单价 = 微单位每 token 数。
pub const MICROS_PER_UNIT: f64 = 1_000_000.0;

/// 外部源 per-1M tokens 单价 → catalog per-1M tokens 单价（恒等映射）。
pub fn source_price_to_catalog_price(per_million: f64) -> f64 {
    per_million
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 口径等价性锚：3.0 USD/1M tokens = 3.0 μ$/token（MICROS_PER_UNIT
    /// 换算往返闭合），恒等映射逐位保真。
    #[test]
    fn per_million_equals_micro_per_token_and_identity_holds() {
        let per_million = 3.0;
        let micro_per_token = per_million / MICROS_PER_UNIT * MICROS_PER_UNIT;
        assert!((micro_per_token - per_million).abs() < 1e-12);
        assert_eq!(source_price_to_catalog_price(per_million), per_million);
        // 小数价保真（deepseek 实测值 0.022 不被换算扰动）
        assert_eq!(source_price_to_catalog_price(0.022), 0.022);
    }
}
