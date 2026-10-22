//! Drives `AiClient` against a one-shot local HTTP server that replays a
//! canned response, checking the request it sent.

use spark_ai::{AiClient, AiError, ChatRequest, Message, ProviderConfig, StopReason};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one response and returns the raw request it received.
async fn serve_once(
    status: &str,
    content_type: &str,
    body: &str,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        // Read headers, then as much body as content-length announces.
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        String::from_utf8(request).unwrap()
    });
    (url, handle)
}

fn chat() -> ChatRequest {
    ChatRequest {
        system: None,
        messages: vec![Message::user("Say hi")],
        max_tokens: 100,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_request_and_stream() {
    let (url, server) = serve_once(
        "200 OK",
        "text/event-stream",
        concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":3}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi!\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    )
    .await;
    let config = ProviderConfig {
        base_url: url,
        ..ProviderConfig::anthropic()
    };
    let client = AiClient::new(config, Some("test-key".into())).unwrap();

    let mut streamed = String::new();
    let done = client
        .complete(&chat(), |t| streamed.push_str(t))
        .await
        .unwrap();
    assert_eq!(streamed, "Hi!");
    assert_eq!(done.stop_reason, StopReason::EndTurn);

    let request = server.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with("post /v1/messages "));
    assert!(request.contains("x-api-key: test-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));
    // Not Anthropic's own API, so no fallback beta.
    assert!(!request.contains("anthropic-beta"));
    assert!(request.contains("\"model\":\"claude-opus-5-5\""));
}

#[tokio::test(flavor = "current_thread")]
async fn openai_compatible_request_and_stream() {
    let (url, server) = serve_once(
        "200 OK",
        "text/event-stream",
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hey\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
    )
    .await;
    let config = ProviderConfig::openai_compatible(format!("{url}/v1"), "llama3.2");
    let client = AiClient::new(config, None).unwrap();

    let done = client.complete(&chat(), |_| {}).await.unwrap();
    assert_eq!(done.text, "Hey");

    let request = server.await.unwrap().to_ascii_lowercase();
    assert!(request.starts_with("post /v1/chat/completions "));
    assert!(!request.contains("authorization:"));
    assert!(request.contains("\"max_tokens\":100"));
}

#[tokio::test(flavor = "current_thread")]
async fn http_errors_carry_the_provider_message() {
    let (url, _server) = serve_once(
        "401 Unauthorized",
        "application/json",
        "{\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"}}",
    )
    .await;
    let config = ProviderConfig {
        base_url: url,
        ..ProviderConfig::anthropic()
    };
    let client = AiClient::new(config, Some("wrong".into())).unwrap();

    match client.complete(&chat(), |_| {}).await {
        Err(AiError::Http { status, message }) => {
            assert_eq!(status, 401);
            assert_eq!(message, "invalid x-api-key");
        }
        other => panic!("expected an HTTP error, got {other:?}"),
    }
}
