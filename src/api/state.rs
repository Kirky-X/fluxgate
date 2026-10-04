// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT
// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

use crate::protocol::ProtocolConverter;
use crate::router::affinity::AffinityTable;
use crate::router::bandit::{linucb_features_from_body, BanditState};
use crate::router::Router as RouterTrait;
use crate::storage::{RequestLogSink, UsageSink};
use crate::{ConfigManager, VendorManager};
use std::sync::Arc;

use super::response_cache::ResponseCache;

#[derive(Clone)]
pub struct ApiState {
    pub config: Arc<ConfigManager>,
    pub vendor_manager: Arc<VendorManager>,
    pub router: Arc<RouterTrait>,
    pub openai_converter: Arc<dyn ProtocolConverter>,
    pub anthropic_converter: Arc<dyn ProtocolConverter>,
    /// 请求日志存储后端（N2 存储缝）：admin 读（logs/search/token_stats）
    /// 与直写兜底经 trait 注入，bootstrap 恒接线 Database 实现
    pub database: Arc<dyn RequestLogSink>,
    /// recId=9②：异步日志管道前沿 — handler 对其 `log()` push 即返回
    /// （try_send 背压，满即丢并计数）；None = 未接线（部分测试场景降级
    /// 为不落日志）。
    pub logger: Option<Arc<dyn RequestLogSink>>,
    /// recId=9⑤：Token 使用预聚合前沿 — handler 拿到 usage 后 record_raw，
    /// 定时批量落 token_usage 表；None = 未接线。
    pub pre_aggregator: Option<Arc<dyn UsageSink>>,
    /// recId=11：会话亲和表（oxcache）— 路由 `route_with_session` 的
    /// boost 来源；None = 未接线（亲和关闭，路由行为与历史一致）。
    pub affinity: Option<Arc<AffinityTable>>,
    /// recId=12：精确响应缓存（oxcache）— enabled 由 [cache] 段热读控制
    ///（默认关闭）；None = 未接线。
    pub response_cache: Option<Arc<ResponseCache>>,
    /// recId=15：LinUCB 状态 — selection="linucb" 档的臂状态；Router 与
    /// handler（奖励回传）共享同一实例。None = 未接线（该档回退 weighted、
    /// 奖励不记账，默认行为与历史一致）。
    pub bandit: Option<Arc<BanditState>>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState")
            .field("config", &"ConfigManager")
            .field("vendor_manager", &"VendorManager")
            .field("router", &"Router")
            .field("openai_converter", &"ProtocolConverter")
            .field("anthropic_converter", &"ProtocolConverter")
            .field("database", &"RequestLogSink")
            .field("logger", &self.logger.is_some())
            .field("pre_aggregator", &self.pre_aggregator.is_some())
            .field("affinity", &self.affinity.is_some())
            .field("response_cache", &self.response_cache.is_some())
            .field("bandit", &self.bandit.is_some())
            .finish()
    }
}

impl ApiState {
    /// recId=15：LinUCB 奖励回传 — 落库记账点（成功/失败收敛处）调用。
    /// **门控**：仅 `[routing].selection = "linucb"`（热读）且 bandit 接线
    /// 时 update（r = 成功 1 / 失败 0）；默认 weighted 档无状态更新
    ///（defaultOff 语义）。
    ///
    /// recId=15（评审修复 major）：`arm_key` 由调用方经
    /// `bandit::linucb_arm_key` 归一（与选择侧 route_inner 同源——
    /// lowercase + auto→"__global__"），修复前奖励写原始
    /// `request.get("model")`：auto 路径写 ("auto", vendor) 与被咨询臂
    /// ("__global__", vendor) 永不相交（主路径永不学习）、大写 model
    /// 同理错键。`None` = 直连 vendor 路径（X-Vendor，try_vendor_route
    /// 单候选）——臂未被咨询，跳过奖励（与「未选择不更新」语义一致）。
    /// 特征长度/有限性由 [`BanditState::update`] 兜底（脏样本丢弃）。
    pub fn record_linucb_reward(
        &self,
        arm_key: Option<&str>,
        vendor_id: &str,
        request: &serde_json::Value,
        success: bool,
    ) {
        let Some(bandit) = self.bandit.as_ref() else {
            return;
        };
        if !self
            .config
            .read()
            .routing
            .selection
            .trim()
            .eq_ignore_ascii_case("linucb")
        {
            return; // 默认 weighted 档：不计算特征、不记账（零开销）
        }
        let Some(key) = arm_key else {
            return; // 臂未被咨询（直连路径）：不产生伪样本
        };
        let features = linucb_features_from_body(request);
        bandit.update(key, vendor_id, &features, if success { 1.0 } else { 0.0 });
    }
}
