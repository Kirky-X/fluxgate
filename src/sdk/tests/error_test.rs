// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! SDK 错误体系单测：ApiError 解析（header/body）、分类、可重试判定、
//! 超时/网络分流。

use crate::sdk::error::{ApiErrorKind, FluxgateSdkError};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

#[test]
fn api_display_contains_status_and_message() {
    let err = FluxgateSdkError::from_http_error(500, &HeaderMap::new(), "boom");
    let s = err.to_string();
    assert!(s.contains("500"), "{s}");
    assert!(s.contains("boom"), "{s}");
}

/// 上游错误面不可信：超长 JSON `error.message` 截断到 512 字符
/// （防多 MB 文本随 `Display`/`Debug` 进入调用方日志）。
#[test]
fn oversized_error_message_is_truncated() {
    let huge = "x".repeat(10_000);
    let body = format!(r#"{{"error":{{"message":"{huge}"}}}}"#);
    let err = FluxgateSdkError::from_http_error(500, &HeaderMap::new(), &body);
    match &err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.message.chars().count(), 512);
            assert!(api.message.starts_with("xxx"));
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn auth_error_display_via_api_variant() {
    let err = FluxgateSdkError::from_http_error(
        401,
        &HeaderMap::new(),
        r#"{"error":{"message":"bad key"}}"#,
    );
    match &err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.status, 401);
            assert_eq!(api.kind, ApiErrorKind::Authentication);
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    // Display 含状态与消息（原 Auth 面断言迁移）
    let s = err.to_string();
    assert!(s.contains("bad key"), "{s}");
}

#[test]
fn structured_body_and_headers_are_parsed() {
    let headers = header_map(&[
        ("retry-after", "7"),
        ("x-request-id", "req_123"),
        ("x-should-retry", "true"),
    ]);
    let err = FluxgateSdkError::from_http_error(
        429,
        &headers,
        r#"{"error":{"message":"rate limited","type":"rate_limit_error","code":"rate_limit","param":"input"}}"#,
    );
    match err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.status, 429);
            assert_eq!(api.kind, ApiErrorKind::RateLimit);
            assert_eq!(api.message, "rate limited");
            assert_eq!(api.code.as_deref(), Some("rate_limit"));
            assert_eq!(api.param.as_deref(), Some("input"));
            assert_eq!(api.request_id.as_deref(), Some("req_123"));
            assert_eq!(api.retry_after, Some(Duration::from_secs(7)));
            assert_eq!(api.should_retry, Some(true));
            assert!(api.is_retryable());
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn anthropic_request_id_and_millis_retry_after() {
    let headers = header_map(&[
        ("anthropic-request-id", "req_anthropic"),
        ("retry-after-ms", "1500"),
        ("retry-after", "9"),
    ]);
    let err = FluxgateSdkError::from_http_error(529, &headers, "");
    match err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.kind, ApiErrorKind::Overloaded);
            assert_eq!(api.request_id.as_deref(), Some("req_anthropic"));
            assert_eq!(
                api.retry_after,
                Some(Duration::from_millis(1500)),
                "retry-after-ms 优先于 retry-after"
            );
            assert_eq!(api.message, "http 529", "空 body 回落状态占位");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn retry_after_http_date_is_parsed() {
    // 远期 HTTP-date（IMF-fixdate）
    let headers = header_map(&[("retry-after", "Fri, 01 Jan 2100 00:00:00 GMT")]);
    let err = FluxgateSdkError::from_http_error(503, &headers, "x");
    match err {
        FluxgateSdkError::Api(api) => {
            let remaining = api.retry_after.expect("远期日期可解析");
            assert!(remaining > Duration::from_secs(3600), "{remaining:?}");
        }
        other => panic!("expected Api error, got {other:?}"),
    }

    // 已过的日期 → None（无有用提示）
    let headers = header_map(&[("retry-after", "Mon, 01 Jan 2001 00:00:00 GMT")]);
    let err = FluxgateSdkError::from_http_error(503, &headers, "x");
    match err {
        FluxgateSdkError::Api(api) => assert!(api.retry_after.is_none()),
        other => panic!("expected Api error, got {other:?}"),
    }

    // 不可解析 → None
    let headers = header_map(&[("retry-after", "not-a-date")]);
    let err = FluxgateSdkError::from_http_error(503, &headers, "x");
    match err {
        FluxgateSdkError::Api(api) => assert!(api.retry_after.is_none()),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn non_json_body_falls_back_to_raw_text() {
    let err = FluxgateSdkError::from_http_error(502, &HeaderMap::new(), "<html>bad gateway</html>");
    match err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.kind, ApiErrorKind::Server);
            assert_eq!(api.message, "<html>bad gateway</html>");
            assert!(api.code.is_none() && api.param.is_none());
            assert!(api.retry_after.is_none());
            assert!(api.should_retry.is_none());
        }
        other => panic!("expected Api error, got {other:?}"),
    }

    // error 为字符串的兼容形态
    let err = FluxgateSdkError::from_http_error(400, &HeaderMap::new(), r#"{"error":"plain"}"#);
    match err {
        FluxgateSdkError::Api(api) => assert_eq!(api.message, "plain"),
        other => panic!("expected Api error, got {other:?}"),
    }

    // 超长文本截断 512 字符
    let long = "x".repeat(1000);
    let err = FluxgateSdkError::from_http_error(500, &HeaderMap::new(), &long);
    match err {
        FluxgateSdkError::Api(api) => assert_eq!(api.message.len(), 512),
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn kind_from_status_matrix() {
    for (status, expected) in [
        (400, ApiErrorKind::InvalidRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::Permission),
        (404, ApiErrorKind::NotFound),
        (409, ApiErrorKind::Conflict),
        (413, ApiErrorKind::PayloadTooLarge),
        (422, ApiErrorKind::Unprocessable),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::Server),
        (503, ApiErrorKind::ServiceUnavailable),
        (529, ApiErrorKind::Overloaded),
        (599, ApiErrorKind::Server),
        (302, ApiErrorKind::Other),
    ] {
        let err = FluxgateSdkError::from_http_error(status, &HeaderMap::new(), "x");
        match err {
            FluxgateSdkError::Api(api) => {
                assert_eq!(api.kind, expected, "status {status}");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}

#[test]
fn is_retryable_status_and_header_matrix() {
    // status 派生
    for (status, retryable) in [
        (400, false),
        (404, false),
        (408, true),
        (409, true),
        (429, true),
        (500, true),
        (529, true),
    ] {
        let err = FluxgateSdkError::from_http_error(status, &HeaderMap::new(), "x");
        match err {
            FluxgateSdkError::Api(api) => {
                assert_eq!(api.is_retryable(), retryable, "status {status}");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    // x-should-retry 显式否决优先（500 也不重试）
    let err =
        FluxgateSdkError::from_http_error(500, &header_map(&[("x-should-retry", "false")]), "x");
    match err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.should_retry, Some(false));
            assert!(!api.is_retryable(), "显式 false 优先否决");
        }
        other => panic!("expected Api error, got {other:?}"),
    }

    // x-should-retry 显式放行（400 也重试）
    let err =
        FluxgateSdkError::from_http_error(400, &header_map(&[("x-should-retry", "true")]), "x");
    match err {
        FluxgateSdkError::Api(api) => assert!(api.is_retryable(), "显式 true 放行"),
        other => panic!("expected Api error, got {other:?}"),
    }

    // 无效值 → None（回落 status 判定）
    let err =
        FluxgateSdkError::from_http_error(400, &header_map(&[("x-should-retry", "maybe")]), "x");
    match err {
        FluxgateSdkError::Api(api) => {
            assert_eq!(api.should_retry, None);
            assert!(!api.is_retryable());
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[test]
fn from_reqwest_error_implemented() {
    // 编译时验证 From<reqwest::Error> trait impl 存在（超时/网络手写分流）
    fn assert_from_impl<T, U>()
    where
        U: From<T>,
    {
    }
    assert_from_impl::<reqwest::Error, FluxgateSdkError>();
}
