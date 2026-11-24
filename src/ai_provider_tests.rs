use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
};

fn config(kind: ProviderKind) -> AiConfig {
    AiConfig {
        enabled: true,
        kind,
        base_url: kind.default_url().into(),
        model: "vision".into(),
    }
}

#[test]
fn base64_rfc4648() {
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(plain.as_bytes()), encoded);
    }
    assert_eq!(base64(&[251, 255, 255]), "+///");
}

#[test]
fn request_formats_and_secret_headers() {
    let key = ApiKey::new("sample-secret").unwrap();
    for kind in ProviderKind::ALL {
        let mut cfg = config(kind);
        cfg.base_url.push_str("///");
        cfg.model = "vision/uno ?".into();
        let request = build_request(&cfg, Some(&key), "Test", &["abcd".into()], false).unwrap();
        let body: Value = serde_json::from_slice(request.body()).unwrap();
        assert_eq!(request.method(), "POST");
        assert!(!request.uri().to_string().contains("sample-secret"));
        assert!(!format!("{request:?}").contains("sample-secret"));
        let (path, header, value) = match kind {
            ProviderKind::OpenAiCompatible => {
                assert_eq!(body["model"], "vision/uno ?");
                assert_eq!(body["temperature"], 0.2);
                assert_eq!(body["max_tokens"], 1500);
                assert_eq!(body["messages"][0]["role"], "user");
                assert_eq!(
                    body["messages"][0]["content"][0],
                    json!({"type":"text","text":"Test"})
                );
                assert_eq!(
                    body["messages"][0]["content"][1],
                    json!({"type":"image_url","image_url":{"url":"data:image/jpeg;base64,abcd"}})
                );
                (
                    "/v1/chat/completions",
                    "authorization",
                    "Bearer sample-secret",
                )
            }
            ProviderKind::Anthropic => {
                assert_eq!(request.headers()["anthropic-version"], "2023-06-01");
                assert_eq!(body["max_tokens"], 1500);
                assert_eq!(
                    body["messages"][0]["content"][0],
                    json!({"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"abcd"}})
                );
                assert_eq!(
                    body["messages"][0]["content"][1],
                    json!({"type":"text","text":"Test"})
                );
                ("/v1/messages", "x-api-key", "sample-secret")
            }
            ProviderKind::Gemini => {
                assert_eq!(
                    body["contents"][0]["parts"],
                    json!([{"text":"Test"},{"inline_data":{"mime_type":"image/jpeg","data":"abcd"}}])
                );
                assert_eq!(
                    body["generationConfig"],
                    json!({"temperature":0.2,"responseMimeType":"application/json"})
                );
                (
                    "/v1beta/models/vision%2Funo%20%3F:generateContent",
                    "x-goog-api-key",
                    "sample-secret",
                )
            }
        };
        assert_eq!(request.uri().path(), path);
        assert_eq!(request.headers()[header], value);
        assert!(request.headers()[header].is_sensitive());
    }
    let local = build_request(
        &config(ProviderKind::OpenAiCompatible),
        None,
        "Reply with OK",
        &[],
        true,
    )
    .unwrap();
    assert!(!local.headers().contains_key("authorization"));
    let gemini = build_request(
        &config(ProviderKind::Gemini),
        Some(&key),
        "Reply with OK",
        &[],
        true,
    )
    .unwrap();
    let body: Value = serde_json::from_slice(gemini.body()).unwrap();
    assert_eq!(
        body["contents"][0]["parts"],
        json!([{"text":"Reply with OK"}])
    );
    assert!(body["generationConfig"].get("responseMimeType").is_none());
}

#[test]
fn parses_provider_responses_and_validates_verdict() {
    let reply = concat!(
        "Risposta: ```json\n",
        r#"{"clips":[{"index":0,"score":185,"scene":"nature","keep":false},
        {"index":0,"score":9,"scene":"beach"},{"index":-1,"score":90},{"index":25},
        {"index":1,"score":-2,"scene":"unknown","keep":true},{"index":2,"score":"unknown"}],
        "template":"A tempo","filter":"Warm","title":"Al mare"}"#,
        "\n```"
    );
    let split = reply.find("\"template\"").unwrap();
    for (kind, body) in [
        (
            ProviderKind::OpenAiCompatible,
            json!({"choices":[{"message":{"content":reply}}]}),
        ),
        (
            ProviderKind::Anthropic,
            json!({"content":[{"type":"text","text":&reply[..split]},{"type":"text","text":&reply[split..]}]}),
        ),
        (
            ProviderKind::Gemini,
            json!({"candidates":[{"content":{"parts":[{"text":&reply[..split]},{"text":&reply[split..]}]}}]}),
        ),
    ] {
        let result = parse_verdict(&response_text(kind, &body).unwrap(), 3).unwrap();
        assert_eq!(result.clips.len(), 3);
        assert_eq!(
            result.clips[0],
            ClipVerdict {
                index: 0,
                score: Some(100),
                scene: Some(Scene::Nature),
                keep: Some(false)
            }
        );
        assert_eq!(
            result.clips[1],
            ClipVerdict {
                index: 1,
                score: Some(0),
                scene: None,
                keep: Some(true)
            }
        );
        assert_eq!(result.clips[2].score, None);
        assert_eq!(result.template, Some(Template::InSync));
        assert_eq!(result.filter, Some(Filter(9)));
        assert_eq!(result.title.as_deref(), Some("Al mare"));
    }
    let unknown = parse_verdict(
        r#"Premessa {testo} {"clips":[],"template":"X","filter":"Y"}"#,
        0,
    )
    .unwrap();
    assert_eq!(unknown.template, None);
    assert_eq!(unknown.filter, None);
    assert!(parse_verdict("nessun verdetto", 2).is_err());
}

#[test]
fn accepts_only_https_or_exact_loopback() {
    for url in [
        "http://localhost:1234/v1",
        "http://127.0.0.1",
        "http://[::1]:8080",
        "https://example.com/v1",
    ] {
        assert!(validate_url(url).is_ok(), "{url}");
    }
    for url in [
        "http://example.com",
        "http://localhost.example.com",
        "http://127.0.0.2",
        "http://localhost@example.com",
        "https://user:password@example.com",
        "https://example.com?key=abc",
        "https://example.com/#fragment",
        "ftp://localhost",
        "/v1",
    ] {
        assert!(validate_url(url).is_err(), "{url}");
    }
}

#[test]
fn http_error_redacts_before_truncating() {
    let key = ApiKey::new("sample-secret").unwrap();
    let body = format!(
        "sample-secret sample-secret {}sample-secret",
        "è".repeat(400)
    );
    let error = http_error(401, body.as_bytes(), Some(&key));
    let text = error.strip_prefix("HTTP 401: ").unwrap();
    assert!(text.starts_with("••• ••• "));
    assert_eq!(text.chars().count(), 300);
    assert!(!error.contains("sample-secret"));
}

#[test]
fn thumbnails_use_local_rank_and_jpeg_dimensions() {
    let size = (640, 320);
    let rgb = vec![180; size.0 * size.1 * 3];
    let candidates: Vec<_> = (0..14)
        .map(|index| Thumbnail {
            score: index,
            rgb: &rgb,
            size,
        })
        .chain([Thumbnail {
            score: 100,
            rgb: &[],
            size,
        }])
        .collect();
    let indices = ranked_indices(&candidates);
    assert_eq!(indices, (2..14).rev().collect::<Vec<_>>());
    let jpeg = encode_jpeg(&candidates[indices[0]]).unwrap();
    let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (512, 256));
}

#[test]
fn openai_round_trip_on_loopback() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut cfg = config(ProviderKind::OpenAiCompatible);
    cfg.base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "POST /v1/chat/completions HTTP/1.1\r\n");
        let mut authorized = false;
        let mut length = 0;
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.split_once(':').unwrap();
            if name.eq_ignore_ascii_case("authorization") {
                authorized = value.trim() == "Bearer mock-loopback-key";
            }
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert!(authorized);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert!(body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        let reply = json!({"choices":[{"message":{"content":r#"{"clips":[{"index":0,"score":82,"scene":"snow","keep":true}],"title":"Neve"}"#}}]}).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
    });
    let client = AiClient::new(&cfg, Some(&ApiKey::new("mock-loopback-key").unwrap()));
    let verdict = client
        .request_verdict(&[Thumbnail {
            score: 60,
            rgb: &[15; 12],
            size: (2, 2),
        }])
        .unwrap();
    server.join().unwrap();
    assert_eq!(verdict.clips[0].score, Some(82));
    assert_eq!(verdict.clips[0].scene, Some(Scene::Snow));
}

#[test]
fn dpapi_round_trip_and_removal() {
    let path = std::env::temp_dir()
        .join(format!("studio360-key-test-{}", std::process::id()))
        .join("ai-key.bin");
    let key = ApiKey::new("mock-dpapi-secret-è").unwrap();
    assert!(load_key(&path).unwrap().is_none());
    save_key(&path, &key).unwrap();
    let encrypted = fs::read(&path).unwrap();
    assert!(!encrypted
        .windows(key.0.len())
        .any(|bytes| bytes == key.0.as_bytes()));
    assert!(load_key(&path)
        .unwrap()
        .is_some_and(|loaded| loaded.0 == key.0));
    let replacement = ApiKey::new("new-mock-dpapi-secret").unwrap();
    save_key(&path, &replacement).unwrap();
    assert!(load_key(&path)
        .unwrap()
        .is_some_and(|loaded| loaded.0 == replacement.0));
    remove_key(&path).unwrap();
    remove_key(&path).unwrap();
    assert!(load_key(&path).unwrap().is_none());
    fs::remove_dir(path.parent().unwrap()).unwrap();
}
