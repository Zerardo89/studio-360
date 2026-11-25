use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
};

fn config(kind: ProviderKind) -> AiConfig {
    AiConfig {
        enabled: true,
        kind,
        base_url: kind.default_url().into(),
        model: "vision".into(),
        ..AiConfig::default()
    }
}

fn request_bytes(cfg: &AiConfig, test: bool) -> Vec<u8> {
    let key = ApiKey::new("sample-secret").unwrap();
    let images = if test { vec![] } else { vec!["abcd".into()] };
    build_request(cfg, Some(&key), "Test", &images, test)
        .unwrap()
        .into_body()
}

fn request_body(cfg: &AiConfig, test: bool) -> Value {
    serde_json::from_slice(&request_bytes(cfg, test)).unwrap()
}

#[test]
fn old_config_defaults_to_model_effort() {
    let cfg: AiConfig = serde_json::from_value(json!({
        "enabled":true,"kind":"gemini","base_url":"https://example.com/v1","model":"vision"
    }))
    .unwrap();
    assert_eq!(cfg.effort, Effort::Default);
    assert_eq!(serde_json::to_value(cfg).unwrap()["effort"], "default");
}

#[test]
fn default_effort_preserves_entire_request_body() {
    for test in [false, true] {
        let mut openai_content = vec![json!({"type":"text","text":"Test"})];
        let mut anthropic_content = Vec::new();
        let mut gemini_parts = vec![json!({"text":"Test"})];
        let mut generation = json!({"temperature":0.2});
        if !test {
            openai_content.push(
                json!({"type":"image_url","image_url":{"url":"data:image/jpeg;base64,abcd"}}),
            );
            anthropic_content.push(json!({"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"abcd"}}));
            gemini_parts.push(json!({"inline_data":{"mime_type":"image/jpeg","data":"abcd"}}));
            generation["responseMimeType"] = json!("application/json");
        }
        anthropic_content.push(json!({"type":"text","text":"Test"}));
        for (kind, expected) in [
            (
                ProviderKind::OpenAiCompatible,
                json!({"model":"vision","temperature":0.2,"max_tokens":1500,"messages":[{"role":"user","content":openai_content}]}),
            ),
            (
                ProviderKind::Anthropic,
                json!({"model":"vision","max_tokens":1500,"messages":[{"role":"user","content":anthropic_content}]}),
            ),
            (
                ProviderKind::Gemini,
                json!({"contents":[{"parts":gemini_parts}],"generationConfig":generation}),
            ),
        ] {
            assert_eq!(
                request_bytes(&config(kind), test),
                expected.to_string().into_bytes(),
                "{kind:?}, test={test}"
            );
        }
    }
}

#[test]
fn explicit_effort_uses_provider_specific_fields() {
    for (effort, name, budget) in [
        (Effort::Low, "low", 1024),
        (Effort::Medium, "medium", 8192),
        (Effort::High, "high", 24576),
    ] {
        for test in [false, true] {
            let mut cfg = config(ProviderKind::OpenAiCompatible);
            cfg.effort = effort;
            let body = request_body(&cfg, test);
            assert_eq!(body["reasoning_effort"], name);
            assert_eq!(body["max_completion_tokens"], 16000);
            assert!(body.get("temperature").is_none());
            assert!(body.get("max_tokens").is_none());

            cfg = config(ProviderKind::Anthropic);
            cfg.effort = effort;
            let body = request_body(&cfg, test);
            assert_eq!(body["output_config"], json!({"effort":name}));
            assert_eq!(body["max_tokens"], 8000);

            cfg = config(ProviderKind::Gemini);
            cfg.effort = effort;
            cfg.model = "gemini-2.5-pro".into();
            let body = request_body(&cfg, test);
            assert_eq!(
                body["generationConfig"]["thinkingConfig"],
                json!({"thinkingBudget":budget})
            );
            assert_eq!(
                body["generationConfig"].get("responseMimeType").is_none(),
                test
            );
            cfg.model = "gemini-3-pro".into();
            let body = request_body(&cfg, test);
            assert_eq!(
                body["generationConfig"]["thinkingConfig"],
                json!({"thinkingLevel":name})
            );
        }
    }
}

#[test]
fn anthropic_thinking_is_ignored() {
    let reply = json!({"content":[
        {"type":"thinking","thinking":"ragionamento","text":"non includere"},
        {"type":"text","text":"O"},
        {"type":"redacted_thinking","text":"non includere"},
        {"type":"text","text":"K"}
    ]});
    assert_eq!(
        response_text(ProviderKind::Anthropic, &reply).unwrap(),
        "OK"
    );
}

fn ids(models: &[ModelInfo]) -> Vec<&str> {
    models.iter().map(|model| model.id.as_str()).collect()
}

#[test]
fn parses_anthropic_models_in_service_order_and_continues_after_last_id() {
    let body = json!({"data":[
        {"id":"claude-new","display_name":"Claude nuovo"},
        {"id":"claude-old"},
        {"id":"claude-new"}
    ],"has_more":true,"last_id":"claude-old"});
    let kind = ProviderKind::Anthropic;
    let (models, next) = parse_models(kind, "api.anthropic.com", &body);
    assert_eq!(ids(&models), ["claude-new", "claude-old"]);
    assert_eq!(models[0].label, "Claude nuovo");
    assert_eq!(models[1].label, "claude-old");
    assert_eq!(next.as_deref(), Some("claude-old"));
    let mut final_page = body;
    final_page["has_more"] = json!(false);
    assert!(parse_models(kind, "api.anthropic.com", &final_page)
        .1
        .is_none());
    assert_eq!(suggested_model(kind, &models), models.first());
}

#[test]
fn openai_filters_and_sorts_only_its_official_catalog() {
    let body = json!({"data":[
        {"id":"embedding-local","created":100},
        {"id":"gpt-older","created":10},
        {"id":"gpt-new","created":20,"display_name":"GPT nuovo"},
        {"id":"gpt-mini","created":30},
        {"id":"GPT-REALTIME","created":100},
        {"id":"gpt-without-date"}
    ]});
    let kind = ProviderKind::OpenAiCompatible;
    let (models, next) = parse_models(kind, "API.OPENAI.COM", &body);
    assert_eq!(
        ids(&models),
        ["gpt-mini", "gpt-new", "gpt-older", "gpt-without-date"]
    );
    assert_eq!(models[1].label, "GPT nuovo");
    assert_eq!(suggested_model(kind, &models), Some(&models[1]));
    assert_eq!(suggested_model(kind, &models[..1]), models.first());
    assert!(next.is_none());

    let mut cfg = config(kind);
    cfg.base_url = "http://localhost:1234/v1".into();
    let (local, _) = parse_models(kind, "localhost", &body);
    assert_eq!(
        ids(&local),
        [
            "embedding-local",
            "gpt-older",
            "gpt-new",
            "gpt-mini",
            "GPT-REALTIME",
            "gpt-without-date"
        ]
    );
    assert_eq!(cfg.suggested_model(&local), local.first());
}

#[test]
fn official_openai_prefers_a_chat_model_over_specialized_variants() {
    let cfg = config(ProviderKind::OpenAiCompatible);
    let chat = ModelInfo {
        id: "gpt-5".into(),
        label: "GPT-5".into(),
    };
    for id in [
        "gpt-5-pro-2025-10-06",
        "gpt-5-deep-research",
        "gpt-3.5-turbo-instruct",
        "gpt-4o-audio",
        "gpt-4o-search",
    ] {
        let models = [
            ModelInfo {
                id: id.into(),
                label: id.into(),
            },
            chat.clone(),
        ];
        assert_eq!(cfg.suggested_model(&models), Some(&chat), "{id}");
        assert!(cfg.suggested_model(&models[..1]).is_none(), "{id}");
    }
}

#[test]
fn gemini_filters_for_generate_content_and_suggests_latest_stable_pro() {
    let models = [
        ("gemini-2.9-pro", "generateContent"),
        ("gemini-3.10-flash", "generateContent"),
        ("gemini-3.10-pro", "generateContent"),
        ("gemini-3.9-pro", "generateContent"),
        ("gemini-4-pro-preview", "generateContent"),
        ("gemini-4-pro-exp", "generateContent"),
        ("gemini-4-flash-lite", "generateContent"),
        ("gemini-4-live", "generateContent"),
        ("gemini-5-pro", "embedContent"),
        ("gemini-5-pro-image-generation", "generateContent"),
        ("gemini-tts", "generateContent"),
        ("veo", "generateContent"),
    ].map(|(id, method)| json!({"name":format!("models/{id}"),"displayName":format!("Modello {id}"),"supportedGenerationMethods":[method]}));
    let body = json!({"models":models,"nextPageToken":"page & 2"});
    let kind = ProviderKind::Gemini;
    let (models, next) = parse_models(kind, "generativelanguage.googleapis.com", &body);
    assert_eq!(
        ids(&models),
        [
            "gemini-2.9-pro",
            "gemini-3.10-flash",
            "gemini-3.10-pro",
            "gemini-3.9-pro",
            "gemini-4-pro-preview",
            "gemini-4-pro-exp",
            "gemini-4-flash-lite",
            "gemini-4-live"
        ]
    );
    assert_eq!(next.as_deref(), Some("page & 2"));
    let suggested = suggested_model(kind, &models).unwrap();
    assert_eq!(suggested.id, "gemini-3.10-pro");
    assert_eq!(suggested.label, "Modello gemini-3.10-pro");
    assert_eq!(suggested_model(kind, &models[4..]), models.get(4));
    assert!(suggested_model(kind, &[]).is_none());
}

#[test]
fn cleans_model_ids_and_redacts_labels() {
    let boundary = "è".repeat(200);
    let body = json!({"data":[
        {"id":""}, {"id":"   "}, {"id":"control\n"}, {"id": "\tbad"},
        {"id":"è".repeat(201)}, {"id":boundary},
        {"id":" sample "}, {"id":"sample"},
        {"id":"has-sample-secret"}, {"id":"safe","name":"Label sample-secret"}
    ]});
    let (mut models, _) = parse_models(ProviderKind::OpenAiCompatible, "localhost", &body);
    assert_eq!(
        ids(&models),
        [boundary.as_str(), "sample", "has-sample-secret", "safe"]
    );
    let key = ApiKey::new("sample-secret").unwrap();
    protect_model_secrets(&mut models, &key);
    assert_eq!(ids(&models), [boundary.as_str(), "sample", "safe"]);
    assert_eq!(models[2].label, "Label •••");
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
    let key = ApiKey::new(r#"mock-\"-secret"#).unwrap();
    let escaped = json!({"error":key.0}).to_string();
    assert_eq!(
        http_error(401, escaped.as_bytes(), Some(&key)),
        r#"HTTP 401: {"error":"•••"}"#
    );
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
        let body = read_openai_mock_request(&mut stream);
        assert!(body["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        let reply = json!({"choices":[{"message":{"content":r#"{"clips":[{"index":0,"score":82,"scene":"snow","keep":true}],"title":"Neve"}"#}}]});
        write_mock_reply(&mut stream, "200 OK", &reply);
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
fn model_list_reports_auth_errors_on_loopback() {
    for (kind, status) in [
        (ProviderKind::OpenAiCompatible, "401 Unauthorized"),
        (ProviderKind::OpenAiCompatible, "403 Forbidden"),
        (ProviderKind::Anthropic, "403 Forbidden"),
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut cfg = config(kind);
        cfg.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let expected_request = if kind == ProviderKind::Anthropic {
            "GET /v1/models?limit=100 HTTP/1.1\r\n"
        } else {
            "GET /v1/models HTTP/1.1\r\n"
        };
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, expected_request);
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            write_mock_reply(
                &mut stream,
                status,
                &json!({"error":"Missing bearer authentication in header"}),
            );
        });
        let result = list_models(&cfg, None).unwrap_err();
        assert_eq!(
            result,
            "Chiave API mancante o non valida: il servizio non ha concesso l’elenco dei modelli.",
            "{kind:?}, {status}"
        );
        server.join().unwrap();
    }
}

fn read_openai_mock_request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
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
    serde_json::from_slice(&body).unwrap()
}

fn write_mock_reply(stream: &mut TcpStream, status: &str, reply: &Value) {
    let body = reply.to_string();
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
}

#[test]
fn connection_test_retries_once_with_default_effort_after_http_400() {
    for fallback_succeeds in [true, false] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut cfg = config(ProviderKind::OpenAiCompatible);
        cfg.base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        cfg.effort = Effort::Medium;
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            assert_eq!(
                read_openai_mock_request(&mut first)["reasoning_effort"],
                "medium"
            );
            write_mock_reply(
                &mut first,
                "400 Bad Request",
                &json!({"error":"Unsupported reasoning effort"}),
            );
            drop(first);

            let (mut second, _) = listener.accept().unwrap();
            let body = read_openai_mock_request(&mut second);
            assert_eq!(body["max_tokens"], 1500);
            assert!(body.get("reasoning_effort").is_none());
            if fallback_succeeds {
                write_mock_reply(
                    &mut second,
                    "200 OK",
                    &json!({"choices":[{"message":{"content":"OK"}}]}),
                );
            } else {
                write_mock_reply(
                    &mut second,
                    "400 Bad Request",
                    &json!({"error":"mock-loopback-key"}),
                );
            }
        });
        let client = AiClient::new(&cfg, Some(&ApiKey::new("mock-loopback-key").unwrap()));
        let result = client.test_connection();
        if fallback_succeeds {
            assert!(matches!(result, Ok(ConnectionTest::UsedDefaultEffort)));
        } else {
            assert!(matches!(result, Err(error) if error == r#"HTTP 400: {"error":"•••"}"#));
        }
        server.join().unwrap();
    }
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
