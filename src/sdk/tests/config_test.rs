use crate::sdk::config::RequestConfig;
use serde_json::json;

#[test]
fn default_stream_is_false() {
    assert!(!RequestConfig::default().stream);
}

#[test]
fn merge_takes_other_priority() {
    let a = RequestConfig::default();
    let b = RequestConfig {
        temperature: Some(0.5),
        ..Default::default()
    };
    let merged = a.merge(&b);
    assert_eq!(merged.temperature, Some(0.5));
}

#[test]
fn to_openai_json_contains_temperature_and_stream() {
    let config = RequestConfig::default();
    let v = config.to_openai_json();
    assert!(v.get("temperature").is_some());
    assert_eq!(v["stream"], json!(false));
}

#[test]
fn to_openai_json_excludes_hints() {
    let config = RequestConfig {
        vendor_hint: Some("openai".into()),
        model_hint: Some("gpt-4".into()),
        ..Default::default()
    };
    let v = config.to_openai_json();
    assert!(v.get("vendor_hint").is_none());
    assert!(v.get("model_hint").is_none());
}

#[test]
fn to_openai_json_includes_all_fields_when_set() {
    let config = RequestConfig {
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        max_tokens: Some(1000),
        stream: true,
        stop: Some(vec!["\n".into()]),
        user: Some("user-123".into()),
        ..Default::default()
    };
    let v = config.to_openai_json();
    // f32 → f64 有精度损失，用近似比较
    assert!((v["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
    assert!((v["top_p"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(v["top_k"], json!(40));
    assert_eq!(v["max_tokens"], json!(1000));
    assert_eq!(v["stream"], json!(true));
    assert_eq!(v["stop"], json!(["\n"]));
    assert_eq!(v["user"], json!("user-123"));
}
