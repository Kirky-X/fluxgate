// SSE 编码器单元测试

use nexusflow::api::sse::{encode_sse_done, encode_sse_error, encode_sse_event};

#[test]
fn encodes_plain_data_without_event() {
    let out = encode_sse_event("hello", None);
    assert_eq!(out, "data: hello\n\n");
}

#[test]
fn encodes_typed_event_with_chunk() {
    let out = encode_sse_event("{\"x\":1}", Some("chunk"));
    assert_eq!(out, "event: chunk\ndata: {\"x\":1}\n\n");
}

#[test]
fn encodes_done_marker() {
    assert_eq!(encode_sse_done(), "data: [DONE]\n\n");
}

#[test]
fn encodes_error_event_contains_event_and_data() {
    let out = encode_sse_error("boom");
    assert!(out.contains("event: error"));
    assert!(out.contains("data: boom"));
}

#[test]
fn encodes_error_event_terminates_with_double_newline() {
    let out = encode_sse_error("oops");
    assert!(out.ends_with("\n\n"));
}
