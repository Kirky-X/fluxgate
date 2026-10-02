use crate::sdk::stream::ChatStream;
use bytes::Bytes;
use futures::StreamExt;

#[tokio::test]
async fn stream_decodes_chunks_and_filters_done() {
    let frame1 = Bytes::from(
        "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n".to_string(),
    );
    let frame2 = Bytes::from(
        "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\" there\"}}]}\n\n".to_string(),
    );
    let frame3 = Bytes::from("data: [DONE]\n\n".to_string());

    let byte_stream = futures::stream::iter(vec![
        Ok::<bytes::Bytes, reqwest::Error>(frame1),
        Ok::<bytes::Bytes, reqwest::Error>(frame2),
        Ok::<bytes::Bytes, reqwest::Error>(frame3),
    ]);

    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    let mut collected = Vec::new();
    while let Some(chunk) = chat_stream.next().await {
        collected.push(chunk.unwrap());
    }

    assert_eq!(collected.len(), 2, "[DONE] frame should be filtered");
    assert_eq!(collected[0].id, "1");
    assert_eq!(
        collected[0].delta.as_ref().unwrap().content.as_deref(),
        Some("Hi")
    );
    assert_eq!(
        collected[1].delta.as_ref().unwrap().content.as_deref(),
        Some(" there")
    );

    let combined: String = collected
        .iter()
        .filter_map(|c| c.delta.as_ref())
        .filter_map(|d| d.content.clone())
        .collect();
    assert_eq!(combined, "Hi there");
}

#[tokio::test]
async fn stream_propagates_parse_error() {
    let bad_frame = Bytes::from("data: not valid json\n\n".to_string());

    let byte_stream = futures::stream::iter(vec![Ok::<bytes::Bytes, reqwest::Error>(bad_frame)]);

    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    let result = chat_stream.next().await.unwrap();

    assert!(result.is_err());
}

#[tokio::test]
async fn stream_empty_input_yields_nothing() {
    let byte_stream = futures::stream::iter(Vec::<Result<bytes::Bytes, reqwest::Error>>::new());
    let mut chat_stream = ChatStream::from_bytes_stream(byte_stream);
    assert!(chat_stream.next().await.is_none());
}
