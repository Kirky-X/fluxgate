// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

#![cfg(feature = "masking")]

//! 出站掩码引擎测试（feature `masking`）：规则检测器 / 自定义词与正则 /
//! 占位符注册表 / JSON 掩码遍历 / 三档动作 / 流式还原缓冲 / tool 参数增量 /
//! 非流式整量还原 / fail-closed 门。

#[path = "../common/mod.rs"]
mod common;

mod detect {
    use fluxgate::masking::{MaskDetector, MaskKind, RuleDetector};

    fn detector() -> RuleDetector {
        RuleDetector::build(&[], false, &[]).expect("builtin-only detector builds")
    }

    /// 内置形态命中与种类归因：`sk-` API key → ApiKey、PEM 私钥块 →
    /// PrivateKey、email → Email；span 落在原文字节区间内。
    #[test]
    fn detects_builtin_kinds() {
        let d = detector();
        let text = "key sk-proj-abcdefghijklmnopqrstuvwxyz123456 mail a@b.com and\n\
                    -----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\nend";
        let hits = d.detect(text);

        let kinds: Vec<MaskKind> = hits.iter().map(|h| h.kind).collect();
        assert!(
            kinds.contains(&MaskKind::ApiKey),
            "sk- 必须归因 ApiKey: {hits:?}"
        );
        assert!(
            kinds.contains(&MaskKind::PrivateKey),
            "PEM 必须归因 PrivateKey: {hits:?}"
        );
        assert!(
            kinds.contains(&MaskKind::Email),
            "email 必须归因 Email: {hits:?}"
        );

        for hit in &hits {
            let slice = &text[hit.span.clone()];
            assert!(!slice.is_empty(), "命中区间不得为空");
            assert!(text.contains(slice), "命中区间必须落在原文内: {slice}");
        }
        // 精确归因：sk- 命中的区间文本以 sk- 开头；email 命中即 a@b.com
        let sk = hits
            .iter()
            .find(|h| h.kind == MaskKind::ApiKey)
            .expect("sk- 命中");
        assert!(
            text[sk.span.clone()].starts_with("sk-"),
            "{:?}",
            &text[sk.span.clone()]
        );
        let mail = hits
            .iter()
            .find(|h| h.kind == MaskKind::Email)
            .expect("email 命中");
        assert_eq!(&text[mail.span.clone()], "a@b.com");
    }

    /// 干净文本零命中（纯检测不误报）。
    #[test]
    fn clean_text_has_no_hits() {
        let d = detector();
        assert!(d.detect("plain text with no secrets").is_empty());
    }
}

mod custom {
    use fluxgate::masking::{MaskDetector, MaskKind, RuleDetector};

    /// 自定义词与正则命中 Custom；`words_any_case` 时大小写不敏感；
    /// 非法正则构造期 Err（启动拒绝面）。
    #[test]
    fn custom_words_and_rules_hit() {
        // 词命中 → Custom
        let d = RuleDetector::build(&["内部门牌".to_string()], false, &[]).unwrap();
        let hits = d.detect("send 内部门牌 today");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].kind, MaskKind::Custom);
        assert_eq!(&"send 内部门牌 today"[hits[0].span.clone()], "内部门牌");

        // 正则命中 → Custom
        let d = RuleDetector::build(&[], false, &["AKIA[0-9A-Z]{16}".to_string()]).unwrap();
        let hits = d.detect("id AKIAIOSFODNN7EXAMPLE end");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].kind, MaskKind::Custom);
        assert_eq!(
            &"id AKIAIOSFODNN7EXAMPLE end"[hits[0].span.clone()],
            "AKIAIOSFODNN7EXAMPLE"
        );

        // any_case：变体大小写命中；关闭时不命中
        let ci = RuleDetector::build(&["Badge".to_string()], true, &[]).unwrap();
        assert_eq!(ci.detect("see BaDgE here").len(), 1);
        let cs = RuleDetector::build(&["Badge".to_string()], false, &[]).unwrap();
        assert_eq!(cs.detect("see BaDgE here").len(), 0);

        // 非法正则 → 构造期 Err（启动拒绝）
        let err = RuleDetector::build(&[], false, &["(unclosed".to_string()]).unwrap_err();
        assert!(
            matches!(err, fluxgate::masking::MaskError::InvalidRule { .. }),
            "{err:?}"
        );
    }
}

mod placeholder {
    use fluxgate::masking::{MaskKind, PlaceholderRegistry};
    use std::sync::Arc;

    /// 确定性占位符：同值同种类跨注册表实例同串；形态
    /// `{{FLUX_<KIND>_<8HEX>}}`；不同种类同值异串。
    #[test]
    fn same_value_same_placeholder() {
        let r1 = PlaceholderRegistry::new();
        let r2 = PlaceholderRegistry::new();
        let p1 = r1.mask(MaskKind::ApiKey, "sk-proj-abc");
        let p2 = r2.mask(MaskKind::ApiKey, "sk-proj-abc");
        let p3 = r1.mask(MaskKind::ApiKey, "sk-proj-abc");
        assert_eq!(p1, p2, "跨实例确定性");
        assert_eq!(p1, p3, "同实例幂等");
        assert!(
            p1.starts_with("{{FLUX_APIKEY_") && p1.ends_with("}}"),
            "形态固定: {p1}"
        );
        let hex = p1
            .trim_start_matches("{{FLUX_APIKEY_")
            .trim_end_matches("}}");
        assert_eq!(hex.len(), 8, "8 位 hex: {p1}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{p1}");

        // 不同种类同值 → 异串（还原歧义防护）
        let other_kind = r1.mask(MaskKind::Token, "sk-proj-abc");
        assert_ne!(p1, other_kind);
    }

    /// 回环还原：mask 产出的占位符经同一注册表 restore 回原值；
    /// 未知/非占位符串还原 None。
    #[test]
    fn restore_roundtrip() {
        let r = PlaceholderRegistry::new();
        let secret = "super-secret-.env/with\"quotes\"and\\backslash";
        let p = r.mask(MaskKind::ApiKey, secret);
        assert_eq!(r.restore(&p).as_deref(), Some(secret));
        assert_eq!(r.restore("{{FLUX_APIKEY_9F3C21D7}}"), None, "未注册占位符");
        assert_eq!(r.restore("plain text"), None);
    }

    /// LRU 逐出：容量 3 注入 5 条后最早 2 条映射丢失（restore None），
    /// 但同值重掩码可再生同占位符并重建映射（不报错）；最近 3 条保留。
    #[test]
    fn lru_eviction_keeps_recent() {
        let r = Arc::new(PlaceholderRegistry::with_capacity(3));
        let values: Vec<String> = (0..5).map(|i| format!("value-{i}")).collect();
        let placeholders: Vec<String> =
            values.iter().map(|v| r.mask(MaskKind::Custom, v)).collect();

        assert_eq!(r.restore(&placeholders[0]), None, "最早 2 条被逐出");
        assert_eq!(r.restore(&placeholders[1]), None);
        for i in 2..5 {
            assert_eq!(
                r.restore(&placeholders[i]).as_deref(),
                Some(values[i].as_str()),
                "最近 3 条保留"
            );
        }

        // 逐出后重掩码：同占位符重生成 + 映射重建（幂等不报错）
        let again = r.mask(MaskKind::Custom, &values[0]);
        assert_eq!(again, placeholders[0], "确定性重生成");
        assert_eq!(r.restore(&again).as_deref(), Some(values[0].as_str()));
    }
}

mod mask_json {
    use fluxgate::masking::{
        mask_json, ActionMap, AllowList, KindSet, MaskAction, MaskDetector, MaskError, MaskKind,
        MaskTicket, PlaceholderRegistry, RuleDetector,
    };
    use serde_json::json;

    fn walk() -> RuleDetector {
        RuleDetector::build(&["内部门牌".to_string()], false, &[]).expect("detector")
    }

    /// 全量字符串遍历：嵌套 messages/system/tools/tool_result 字符串均被
    /// 替换；数字/布尔不动；命中元数据（种类/长度/前 3 字符）随返回值披露。
    #[test]
    fn mask_json_walks_all_string_values() {
        let mut body = json!({
            "model": "gpt-4",
            "stream": false,
            "max_tokens": 128,
            "messages": [
                {"role": "user", "content": "secret sk-proj-abcdefghijklmnopqrstuvwxyz123456 end"}
            ],
            "system": "leak 内部门牌 tail",
            "tools": [{"function": {"description": "note a@b.com inside"}}],
            "tool_result": {"content": "echo 内部门牌 done"}
        });
        let detector = walk();
        let registry = PlaceholderRegistry::new();
        let infos = mask_json(
            &mut body,
            &detector,
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::default(),
            &registry,
            &MaskTicket::new(),
        )
        .expect("mask ok");

        let rendered = body.to_string();
        assert!(
            !rendered.contains("sk-proj-abcdefghij"),
            "API key 必须替换: {rendered}"
        );
        assert!(rendered.contains("{{FLUX_"), "占位符必须在场: {rendered}");
        assert!(
            !rendered.contains("内部门牌"),
            "词表命中必须替换: {rendered}"
        );
        assert!(!rendered.contains("a@b.com"), "email 必须替换: {rendered}");
        // 非字符串标量与无命中字符串不动
        assert_eq!(body["max_tokens"], json!(128));
        assert_eq!(body["stream"], json!(false));
        assert_eq!(body["model"], json!("gpt-4"));

        // 命中元数据：种类/长度（不含值片段）
        assert_eq!(infos.len(), 4, "4 处命中: {infos:?}");
        let api = infos
            .iter()
            .find(|i| i.kind == MaskKind::ApiKey)
            .expect("api hit");
        assert_eq!(api.len, "sk-proj-abcdefghijklmnopqrstuvwxyz123456".len());
        let custom = infos.iter().filter(|i| i.kind == MaskKind::Custom).count();
        assert_eq!(custom, 2, "两处词命中: {infos:?}");
        let mail = infos
            .iter()
            .find(|i| i.kind == MaskKind::Email)
            .expect("email hit");
        assert_eq!(mail.len, "a@b.com".len());
    }

    /// `allow` 白名单：精确值跳过（值原样、不产命中）。
    #[test]
    fn allow_list_skips_exact_value() {
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        let mut body = json!({"messages": [{"role": "user", "content": format!("x {secret} y")}]});
        let detector = walk();
        let registry = PlaceholderRegistry::new();
        let infos = mask_json(
            &mut body,
            &detector,
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::from_values([secret.to_string()]),
            &registry,
            &MaskTicket::new(),
        )
        .expect("mask ok");
        assert_eq!(infos.len(), 0, "白名单值不产命中: {infos:?}");
        assert!(body.to_string().contains(secret), "白名单值原样保留");
    }

    /// `Block` 动作：整请求拒绝（`MaskError::Blocked`），原文保持由调用方按
    /// 400 拒发。
    #[test]
    fn block_action_rejects() {
        let mut body = json!({"messages": [{"role": "user", "content": "see 内部门牌 here"}]});
        let mut actions = ActionMap::default();
        actions.set(MaskKind::Custom, MaskAction::Block);
        let err = mask_json(
            &mut body,
            &walk(),
            &actions,
            &KindSet::all(),
            &AllowList::default(),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            MaskError::Blocked {
                kind: MaskKind::Custom
            }
        );
        assert!(
            body.to_string().contains("内部门牌"),
            "Block 拒绝时原文保持"
        );
    }

    /// `Warn` 动作：只记元数据不改写。
    #[test]
    fn warn_action_leaves_value() {
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        let mut body = json!({"messages": [{"role": "user", "content": format!("x {secret} y")}]});
        let mut actions = ActionMap::default();
        actions.set(MaskKind::ApiKey, MaskAction::Warn);
        let infos = mask_json(
            &mut body,
            &walk(),
            &actions,
            &KindSet::all(),
            &AllowList::default(),
            &PlaceholderRegistry::new(),
            &MaskTicket::new(),
        )
        .expect("warn ok");
        assert_eq!(infos.len(), 1, "Warn 计数入元数据: {infos:?}");
        assert!(body.to_string().contains(secret), "Warn 不改写值");
    }

    /// `Mask` 动作（缺省）：值替换为占位符。
    #[test]
    fn mask_action_replaces() {
        let mut body = json!({"messages": [{"role": "user", "content": "x sk-proj-abcdefghijklmnopqrstuvwxyz123456 y"}]});
        let mut actions = ActionMap::default();
        actions.set(MaskKind::ApiKey, MaskAction::Mask);
        let registry = PlaceholderRegistry::new();
        let infos = mask_json(
            &mut body,
            &walk(),
            &actions,
            &KindSet::all(),
            &AllowList::default(),
            &registry,
            &MaskTicket::new(),
        )
        .expect("mask ok");
        assert_eq!(infos.len(), 1);
        let rendered = body.to_string();
        assert!(rendered.contains("{{FLUX_APIKEY_"), "{rendered}");
        assert!(!rendered.contains("sk-proj-abcdefghij"), "{rendered}");
    }

    /// `kinds` 未启用的种类不处理（零动作零元数据）。
    #[test]
    fn disabled_kind_is_ignored() {
        let mut body = json!({"messages": [{"role": "user", "content": "x sk-proj-abcdefghijklmnopqrstuvwxyz123456 y"}]});
        let registry = PlaceholderRegistry::new();
        let kinds = KindSet::from_kinds([MaskKind::Custom]);
        let infos = mask_json(
            &mut body,
            &walk(),
            &ActionMap::default(),
            &kinds,
            &AllowList::default(),
            &registry,
            &MaskTicket::new(),
        )
        .expect("mask ok");
        assert_eq!(infos.len(), 0, "未启用种类不产元数据");
        assert!(
            body.to_string()
                .contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
            "未启用种类原样"
        );
    }

    /// 检测器 trait 面向 `dyn` 可用（门注入 panicking 检测器的前置）。
    #[test]
    fn detector_is_object_safe() {
        let d: std::sync::Arc<dyn MaskDetector> = std::sync::Arc::new(walk());
        assert_eq!(d.detect("内部门牌").len(), 1);
    }
}

mod stream_restore {
    use fluxgate::masking::{MaskKind, PlaceholderRegistry, StreamRestorer};
    use std::sync::Arc;

    fn registry() -> Arc<PlaceholderRegistry> {
        Arc::new(PlaceholderRegistry::new())
    }

    fn mask(r: &PlaceholderRegistry, value: &str) -> String {
        r.mask(MaskKind::ApiKey, value)
    }

    /// 占位符拆 chunk 还原：`{{FLUX_APIK` + `EY_XXXX}}` 两段输入，拼接输出
    /// 含原值（跨 chunk 边界的尾部保持缓冲）。
    #[test]
    fn placeholder_split_across_chunks_restored() {
        let reg = registry();
        let secret = "sk-split-boundary-secret-0001";
        let ph = mask(&reg, secret);
        let (a, b) = ph.split_at(ph.len() / 2);
        let mut restorer = StreamRestorer::new(Arc::clone(&reg));
        let out1 = restorer.feed(a);
        let out2 = restorer.feed(b);
        let out3 = restorer.flush();
        let joined = format!("{out1}{out2}{out3}");
        assert!(joined.contains(secret), "拼接输出必须含原值: {joined:?}");
        assert!(!joined.contains("{{FLUX_"), "占位符必须消失: {joined:?}");
    }

    /// 拆在任意字节位置（含首个 `{`、尾部 `}}` 逐字节切分）都完整还原。
    #[test]
    fn placeholder_split_at_every_offset_restored() {
        let reg = registry();
        let secret = "sk-every-offset";
        let ph = mask(&reg, secret);
        for cut in 1..ph.len() {
            let mut restorer = StreamRestorer::new(Arc::clone(&reg));
            let out = format!(
                "{}{}{}",
                restorer.feed(&ph[..cut]),
                restorer.feed(&ph[cut..]),
                restorer.flush()
            );
            assert_eq!(out, secret, "cut={cut} 必须还原");
        }
    }

    /// 纯文本透传：无占位符文本原样输出（byte-equal），缓冲 ≤ 96 字节
    ///（零延迟语义：整段立即可发）。
    #[test]
    fn plain_text_passthrough_low_latency() {
        let reg = registry();
        let mut restorer = StreamRestorer::new(reg);
        let text = "just plain streaming text without any placeholders at all";
        let out = restorer.feed(text);
        assert_eq!(out, text, "无占位符文本原样透传");
        assert!(
            restorer.buffered_len() <= 96,
            "缓冲必须 ≤96: {}",
            restorer.buffered_len()
        );

        // 大段无 `{` 文本也不滞留
        let big = "x".repeat(4096);
        let out = restorer.feed(&big);
        assert_eq!(out, big);
        assert_eq!(restorer.buffered_len(), 0);
    }

    /// 缓冲上界：占位符候选前缀滞留 ≤ 96 字节；补全合法形态时未注册
    /// 占位符原样透传（防伪造占位符丢失）。
    #[test]
    fn buffer_capped_and_candidate_released() {
        let reg = registry();
        let mut restorer = StreamRestorer::new(reg);
        // 悬空候选前缀（永不闭合）：滞留但受上界约束
        let out = restorer.feed("abc{{FLUX_APIK");
        assert_eq!(out, "abc");
        assert!(restorer.buffered_len() <= 96);
        // 补全为合法形态但未注册 → 原样透传
        let done = restorer.feed("EY_DEADBEEF}}");
        assert_eq!(
            format!("{done}{}", restorer.flush()),
            "{{FLUX_APIKEY_DEADBEEF}}"
        );
    }

    /// 混合流：占位符前后夹纯文本，输出逐段正确。
    #[test]
    fn mixed_text_and_placeholder() {
        let reg = registry();
        let secret = "sk-mixed-secret";
        let ph = mask(&reg, secret);
        let mut restorer = StreamRestorer::new(reg);
        let mut out = String::new();
        out.push_str(&restorer.feed("before "));
        out.push_str(&restorer.feed(&ph));
        out.push_str(&restorer.feed(" after"));
        out.push_str(&restorer.flush());
        assert_eq!(out, format!("before {secret} after"));
    }

    /// tool 参数增量：arguments JSON 文档以 3 个片段流入（占位符横跨
    /// 片段 2/3），独立实例还原后拼接含原值（IR 层 arguments 是未转义
    /// 纯文本增量，同内容通道语义）。
    #[test]
    fn tool_argument_deltas_restored() {
        let reg = registry();
        let secret = "sk-tool-arg-secret";
        let ph = mask(&reg, secret);
        let full = format!(r#"{{"command":"run {ph} --now"}}"#);
        // 三段切分（首段完整 JSON 前缀、中段占位符前半、尾段余下）
        let cuts = (14, 14 + ph.len() / 2 + 2);
        let parts = [&full[..cuts.0], &full[cuts.0..cuts.1], &full[cuts.1..]];
        // 每个 tool_call 增量流独占一个 Restorer 实例
        let mut tool_restorer = StreamRestorer::new(reg.clone());
        let mut joined = String::new();
        for part in parts {
            joined.push_str(&tool_restorer.feed(part));
        }
        joined.push_str(&tool_restorer.flush());
        assert!(joined.contains(secret), "tool 参数还原含原值: {joined:?}");
        assert!(joined.contains(r#""command""#), "JSON 结构保持: {joined:?}");
        // 内容通道与工具通道互不串扰：独立实例互不影响
        let mut content = StreamRestorer::new(reg);
        assert_eq!(content.feed("plain"), "plain");
    }
}

mod restore_json {
    use fluxgate::masking::{
        mask_json, placeholder_for, restore_json, ActionMap, AllowList, KindSet, MaskKind,
        MaskTicket, PlaceholderRegistry, RuleDetector,
    };
    use serde_json::json;

    /// mask → restore 回环：响应 Value 递归还原回原值；非占位符字符串
    /// 零改动；未注册占位符原样保留。
    #[test]
    fn restore_json_roundtrip() {
        let secret = "sk-roundtrip-0123456789abcdefghij";
        let mut body = json!({
            "choices": [{"message": {"content": format!("echo {secret} end")}}],
            "usage": {"total_tokens": 42},
            "keep": "text without placeholders",
        });
        let detector = RuleDetector::build(&[], false, &[]).unwrap();
        let registry = PlaceholderRegistry::new();
        let infos = mask_json(
            &mut body,
            &detector,
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::default(),
            &registry,
            &MaskTicket::new(),
        )
        .expect("mask ok");
        assert_eq!(infos.len(), 1);

        let restored_count = restore_json(&mut body, &registry, None);
        assert_eq!(restored_count, 1);
        assert_eq!(
            body["choices"][0]["message"]["content"],
            json!(format!("echo {secret} end")),
            "回环还原原值"
        );
        assert_eq!(
            body["keep"],
            json!("text without placeholders"),
            "非占位符零改动"
        );
        assert_eq!(body["usage"]["total_tokens"], json!(42), "数字不动");
    }

    /// 未注册占位符（如上游编造）原样保留，不报错。
    #[test]
    fn unknown_placeholder_passthrough() {
        let mut body = json!({"content": "see {{FLUX_APIKEY_DEADBEEF}} literal"});
        let registry = PlaceholderRegistry::new();
        let restored_count = restore_json(&mut body, &registry, None);
        assert_eq!(restored_count, 0);
        assert_eq!(
            body["content"],
            json!("see {{FLUX_APIKEY_DEADBEEF}} literal")
        );
    }

    /// 请求域还原（跨请求还原预言机防线）：本请求 ticket 未登记的占位符
    ///——哪怕已在注册表（他请求登记过）——一律不还原；域内命中正常回填。
    /// 伪造/回显他租户占位符的探测面由此闭合。
    #[test]
    fn restore_scopes_to_request_ticket() {
        use fluxgate::masking::MaskTicket;
        let foreign_secret = "sk-tenant-A-0123456789abcdefghij";
        let own_secret = "sk-tenant-B-0123456789abcdefghij";
        let registry = PlaceholderRegistry::new();
        // 他请求：登记 tenant-A 的占位符（生产中 = 进程内注册表残留）
        let _ = registry.mask(MaskKind::ApiKey, foreign_secret);
        // 本请求：只掩码自己的值
        let mut body = json!({"content": format!("x {own_secret} y")});
        let detector = RuleDetector::build(&[], false, &[]).unwrap();
        let ticket = MaskTicket::new();
        mask_json(
            &mut body,
            &detector,
            &ActionMap::default(),
            &KindSet::all(),
            &AllowList::default(),
            &registry,
            &ticket,
        )
        .expect("mask ok");
        assert!(!ticket.is_empty(), "本请求有登记");

        // 回程上游回显他请求的占位符 + 本请求的占位符：只还原本域
        let foreign_ph = placeholder_for(MaskKind::ApiKey, foreign_secret);
        let own_ph = placeholder_for(MaskKind::ApiKey, own_secret);
        let mut response = json!({"content": format!("{foreign_ph} and {own_ph}")});
        let restored = restore_json(&mut response, &registry, Some(&ticket));
        assert_eq!(
            response["content"],
            json!(format!("{foreign_ph} and {own_secret}")),
            "域外占位符不得还原（跨请求预言机闭合），域内正常回填"
        );
        assert_eq!(restored, 1);

        // 对照：不限域（None，SDK/测试面）仍走注册表全量
        let mut response2 = json!({"content": foreign_ph});
        assert_eq!(restore_json(&mut response2, &registry, None), 1);
        assert_eq!(response2["content"], json!(foreign_secret));
    }

    /// MaskKind 的 API 复核：all() 覆盖七个种类（配置闭集单源）。
    #[test]
    fn kind_parse_covers_all() {
        for kind in MaskKind::all() {
            assert_eq!(MaskKind::parse(kind.as_str()), Some(kind));
        }
    }
}

mod gate {
    use fluxgate::config::{Config, MaskingSection};
    use fluxgate::masking::{MaskDetector, MaskError, MaskGate, MaskHit, MaskKind, RuleDetector};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn config_with(masking: MaskingSection) -> Config {
        let mut config = crate::common::test_config(vec![]);
        config.masking = masking;
        config
    }

    /// 恒 panic 检测器（fail-closed 注入面）。
    struct PanickingDetector;

    impl MaskDetector for PanickingDetector {
        fn detect(&self, _text: &str) -> Vec<MaskHit> {
            panic!("detector exploded");
        }
    }

    /// 检测器 panic → `MaskGate::mask_body` 返回 `MaskError::Unavailable`
    ///（调用方按 503 拒发），原文绝不出站。
    #[test]
    fn gate_error_yields_503_not_raw_send() {
        let config = Arc::new(fluxgate::config::ConfigManager::new(config_with(
            MaskingSection {
                enabled: true,
                ..Default::default()
            },
        )));
        let gate = MaskGate::new(
            config,
            Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
        )
        .with_detector(Arc::new(PanickingDetector));
        let body = json!({"messages": [{"role": "user", "content": "hello world"}]});
        let err = gate.mask_body(body).unwrap_err();
        assert_eq!(err, MaskError::Unavailable, "panic 必须折叠为 Unavailable");
    }

    /// enabled=false（默认）：零行为变化——请求原样透传，检测器不被咨询。
    #[test]
    fn disabled_gate_passthrough() {
        let config = Arc::new(fluxgate::config::ConfigManager::new(config_with(
            MaskingSection::default(),
        )));
        let gate = MaskGate::new(
            config,
            Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
        )
        .with_detector(Arc::new(PanickingDetector));
        assert!(!gate.enabled());
        let body = json!({"messages": [{"role": "user", "content": "sk-proj-abcdefghijklmnopqrstuvwxyz123456"}]});
        let (out, ticket) = gate.mask_body(body.clone()).expect("disabled passthrough");
        assert_eq!(out, body, "关闭时零行为变化");
        assert!(ticket.is_empty(), "关闭时零登记（还原端整体跳过）");
    }

    /// 正常路径：enabled + 真检测器 → 掩码生效；restore 回环原值。
    #[test]
    fn gate_masks_and_restores() {
        let config = Arc::new(fluxgate::config::ConfigManager::new(config_with(
            MaskingSection {
                enabled: true,
                ..Default::default()
            },
        )));
        let detector = RuleDetector::build(&[], false, &[]).unwrap();
        let gate = MaskGate::new(
            config,
            Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
        )
        .with_detector(Arc::new(detector));
        let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz123456";
        let body = json!({"messages": [{"role": "user", "content": format!("x {secret} y")}]});
        let (masked, ticket) = gate.mask_body(body).expect("mask ok");
        let rendered = masked.to_string();
        assert!(rendered.contains("{{FLUX_APIKEY_"), "{rendered}");
        assert!(!rendered.contains("sk-proj-abcdefghij"));

        // 本请求登记过的占位符经同一门还原
        let mut response = masked;
        gate.restore_body(&mut response, &ticket)
            .expect("restore ok");
        assert!(response.to_string().contains(secret), "还原回原值");
    }

    /// Block 动作经门 → `MaskError::Blocked`。
    #[test]
    fn gate_propagates_block() {
        let config = Arc::new(fluxgate::config::ConfigManager::new(config_with(
            MaskingSection {
                enabled: true,
                actions: [("custom".to_string(), "block".to_string())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )));
        let detector = RuleDetector::build(&["内部门牌".to_string()], false, &[]).unwrap();
        let gate = MaskGate::new(
            config,
            Arc::new(fluxgate::utils::metrics::GlobalMetrics::new()),
        )
        .with_detector(Arc::new(detector));
        let body = json!({"messages": [{"role": "user", "content": "see 内部门牌 here"}]});
        let err = gate.mask_body(body).unwrap_err();
        assert_eq!(
            err,
            MaskError::Blocked {
                kind: MaskKind::Custom
            }
        );
    }
}
