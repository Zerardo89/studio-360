use super::*;
use crate::ai_cli::{self, CliTool};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
};

#[test]
#[ignore = "usa la CLI Codex installata e l'abbonamento"]
fn live_codex_cli_lists_models_and_reads_a_jpeg() {
    let config = AiConfig {
        kind: ProviderKind::LocalCli,
        cli_tool: CliTool::Codex,
        cli_path: String::new(),
        effort: Effort::Low,
        ..Default::default()
    };

    let executable = ai_cli::resolve_executable(config.cli_tool, &config.cli_path)
        .expect("impossibile trovare la CLI Codex");
    eprintln!("CLI Codex: {}", executable.display());
    assert!(executable.is_file());
    assert!(executable
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("codex.exe")));

    let models = list_models(&config, None).expect("impossibile ottenere i modelli Codex");
    assert!(!models.is_empty(), "il catalogo Codex è vuoto");
    for model in &models {
        assert!(!model.efforts.is_empty(), "nessun livello per {}", model.id);
    }
    for model in models.iter().take(5) {
        eprintln!("Modello Codex: {}", model.id);
    }
    let suggested = config
        .suggested_model(&models)
        .expect("nessun modello Codex suggerito");

    let beach = RgbImage::from_fn(64, 64, |_, y| {
        image::Rgb(if y < 32 {
            [135, 206, 235]
        } else {
            [238, 214, 175]
        })
    });
    let jpeg = encode_jpeg(&Thumbnail {
        score: 0,
        rgb: beach.as_raw(),
        size: (64, 64),
    })
    .expect("impossibile codificare il JPEG di prova");

    let client = AiClient::new(&config, None);
    let prompt = r#"Reply ONLY with JSON: {"scene":"beach|nature|city|snow|sunset|night"}"#;
    let response = client
        .send(prompt, &[jpeg], false)
        .expect("la CLI Codex non ha risposto alla richiesta con il JPEG");
    eprintln!("Risposta Codex: {}", response.text);
    assert!(response.text.contains(r#""scene""#));
    assert_eq!(response.model, suggested.id);
}

#[test]
fn cli_builds_exact_claude_arguments() {
    let default = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--strict-mcp-config",
        "--tools",
        "",
        "--no-session-persistence",
        "--disable-slash-commands",
        "--model",
        "sonnet",
    ];
    assert_eq!(ai_cli::claude_args("sonnet", None).unwrap(), default);
    let mut explicit = default.to_vec();
    explicit.extend(["--effort", "max"]);
    assert_eq!(
        ai_cli::claude_args("sonnet", Some("max")).unwrap(),
        explicit
    );
}

#[test]
fn cli_builds_exact_codex_arguments() {
    let dir = Path::new(r"C:\Temp\Studio 360");
    let jpeg = dir.join("img-01.jpg");
    let jpeg2 = dir.join("img-02.jpg");
    let args = [
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "-s",
        "read-only",
        "--ignore-user-config",
        "--color",
        "never",
        "-C",
        r"C:\Temp\Studio 360",
        "-m",
        "gpt-5.4:vision",
    ];
    let mut default = args.to_vec();
    default.extend(["-o", r"C:\Temp\Studio 360\out.txt", "-"]);
    assert_eq!(
        ai_cli::codex_args(dir, "gpt-5.4:vision", None, &[]).unwrap(),
        default
    );
    let mut explicit = args.to_vec();
    explicit.extend([
        "-c",
        r#"model_reasoning_effort="xhigh""#,
        "-i",
        r"C:\Temp\Studio 360\img-01.jpg",
        "-i",
        r"C:\Temp\Studio 360\img-02.jpg",
        "-o",
        r"C:\Temp\Studio 360\out.txt",
        "-",
    ]);
    assert_eq!(
        ai_cli::codex_args(dir, "gpt-5.4:vision", Some("xhigh"), &[jpeg, jpeg2]).unwrap(),
        explicit
    );
}

#[test]
fn cli_rejects_invalid_model_and_effort_arguments() {
    let directory = Path::new(".");
    for invalid in [
        "",
        "--tools",
        "with space",
        "a\n",
        "x\"y",
        "è",
        &"x".repeat(101),
    ] {
        assert!(ai_cli::claude_args(invalid, None).is_err());
        assert!(ai_cli::codex_args(directory, invalid, None, &[]).is_err());
        assert!(ai_cli::claude_args("sonnet", Some(invalid)).is_err());
        assert!(ai_cli::codex_args(directory, "gpt-5", Some(invalid), &[]).is_err());
    }
    for unsupported in ["ultra", "default", "unknown"] {
        assert!(ai_cli::claude_args("sonnet", Some(unsupported)).is_err());
        assert!(ai_cli::codex_args(directory, "gpt-5", Some(unsupported), &[]).is_err());
    }
}

#[test]
fn codex_catalog_filters_visibility_and_sorts_by_priority() {
    let models = ai_cli::parse_codex_models(
        br#"{
      "models": [
        {"slug":"gpt-later","display_name":"Later","visibility":"list","priority":27,
         "default_reasoning_level":"low","supported_reasoning_levels":[{"effort":"low"}]},
        {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hide","priority":0},
        {"slug":"gpt-suggested","display_name":"Suggested","visibility":"list","priority":3,
         "default_reasoning_level":"high","supported_reasoning_levels":[
           {"effort":"high"},{"effort":"xhigh"},{"effort":"ultra"},{"effort":"max"}]}
      ]
    }"#,
    )
    .unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gpt-suggested", "gpt-later"]
    );
    assert_eq!(models[0].label, "Suggested");
    assert_eq!(
        models[0].efforts,
        [Effort::High, Effort::XHigh, Effort::Max]
    );
    assert!(ai_cli::parse_codex_models(b"{ invalid").is_err());
    assert!(ai_cli::parse_codex_models(b"{}").is_err());
}

#[test]
fn claude_receives_one_user_message_with_jpegs_before_the_prompt() {
    let input = ai_cli::claude_input("Riga 1\nRiga 2", &[vec![0xff, 0xd8, 0xff]]);
    assert_eq!(input.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(input.last(), Some(&b'\n'));
    assert_eq!(
        serde_json::from_slice::<Value>(&input).unwrap(),
        json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"/9j/"}},
                {"type":"text","text":"Riga 1\nRiga 2"}
            ]}
        })
    );
}

#[test]
fn claude_reads_only_the_stream_result_and_reports_errors() {
    let stream = b"{\"type\":\"system\",\"subtype\":\"init\"}\r\n\
        {\"type\":\"assistant\",\"message\":{}}\n\
        {\"type\":\"result\",\"is_error\":false,\"result\":\"Verdetto\"}\n";
    assert_eq!(
        ai_cli::parse_claude_result(stream, b"warning").unwrap(),
        "Verdetto"
    );
    assert_eq!(
        ai_cli::parse_claude_result(
            br#"{"type":"result","is_error":true,"result":"Sessione\u0007 scaduta"}"#,
            b"warning"
        )
        .unwrap_err(),
        "Sessione scaduta"
    );
    assert_eq!(
        ai_cli::parse_claude_result(br#"{"type":"assistant"}"#, b"prima\nultima").unwrap_err(),
        "prima ultima"
    );
    assert!(ai_cli::parse_claude_result(br#"{"type":"assistant"}"#, b"").is_err());
}

#[test]
fn reasoning_menu_and_effective_effort_follow_provider_and_model() {
    use Effort::{Default, High, Low, Max, Medium, XHigh};
    let api = [Default, Low, Medium, High];
    for kind in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ] {
        let mut cfg = config(kind);
        cfg.effort = XHigh;
        assert_eq!(cfg.available_efforts(&[]), api);
        assert_eq!(cfg.effective_effort(&[]), Default);
    }
    let mut cfg = config(ProviderKind::LocalCli);
    cfg.cli_tool = CliTool::Claude;
    cfg.effort = Max;
    let claude = list_models(&cfg, None).unwrap();
    assert_eq!(
        claude.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["fable", "opus", "sonnet", "haiku"]
    );
    assert_eq!(cfg.suggested_model(&claude).unwrap().id, "sonnet");
    assert_eq!(
        cfg.available_efforts(&[]),
        [Default, Low, Medium, High, XHigh, Max]
    );
    assert_eq!(cfg.effective_effort(&[]), Max);
    cfg.cli_tool = CliTool::Codex;
    let codex = vec![
        ModelInfo {
            id: "pinned".into(),
            label: "Pinned".into(),
            efforts: vec![Low, XHigh],
        },
        ModelInfo {
            id: "no-effort".into(),
            label: "No effort".into(),
            efforts: vec![],
        },
    ];
    assert_eq!(cfg.suggested_model(&codex).unwrap().id, "pinned");
    assert_eq!(cfg.available_efforts(&codex), api);
    assert_eq!(cfg.effective_effort(&codex), Default);
    cfg.model = "pinned".into();
    assert_eq!(cfg.available_efforts(&codex), [Default, Low, XHigh]);
    assert_eq!(cfg.effective_effort(&codex), Default);
    cfg.effort = XHigh;
    assert_eq!(cfg.effective_effort(&codex), XHigh);
    cfg.model = "no-effort".into();
    assert_eq!(cfg.available_efforts(&codex), [Default]);
    assert_eq!(cfg.effective_effort(&codex), Default);
    assert_eq!(serde_json::to_value(&cfg).unwrap()["effort"], "xhigh");
}

#[test]
fn cli_validates_the_explicit_executable_without_an_api_url_or_key() {
    ai_cli::with_temp_dir(|dir| {
        let mut cfg = config(ProviderKind::LocalCli);
        cfg.base_url = "invalid API URL".into();

        let script = dir.join("fake.cmd");
        fs::write(&script, b"test").unwrap();
        cfg.cli_path = script.to_string_lossy().into_owned();
        assert!(AiClient::new(&cfg, None)
            .validate()
            .unwrap_err()
            .contains("file .exe"));

        let executable = dir.join("fake.EXE");
        cfg.cli_path = executable.to_string_lossy().into_owned();
        assert!(AiClient::new(&cfg, None)
            .validate()
            .unwrap_err()
            .contains("non esiste"));
        fs::create_dir(&executable).unwrap();
        assert!(AiClient::new(&cfg, None).validate().is_err());
        fs::remove_dir(&executable).unwrap();
        fs::write(&executable, b"test fixture").unwrap();

        cfg.model.clear();
        for tool in CliTool::ALL {
            cfg.cli_tool = tool;
            assert!(AiClient::new(&cfg, None).validate().is_ok());
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn cli_timeout_stops_the_process_and_cleans_up_its_directory() {
    use std::{path::PathBuf, time::Instant};

    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let started = Instant::now();
    let mut directory = PathBuf::new();
    let result = ai_cli::with_temp_dir(|path| {
        directory = path.to_owned();
        fs::write(path.join("img-01.jpg"), b"test").unwrap();
        ai_cli::run_process(
            &powershell,
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ],
            path,
            vec![0; 1024 * 1024],
            ai_cli::RunLimits::new(Duration::from_secs(1), 2 * 1024 * 1024),
        )
        .map(|_| ())
    });
    assert_eq!(
        result.unwrap_err(),
        "il programma AI non ha risposto entro 1 s"
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(!directory.exists());
}

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
fn ai_defaults_to_codex_without_a_manual_model_or_path() {
    let config = AiConfig::default();
    assert!(config.enabled);
    assert_eq!(config.kind, ProviderKind::LocalCli);
    assert_eq!(config.cli_tool, CliTool::Codex);
    assert!(config.base_url.is_empty());
    assert!(config.model.is_empty());
    assert_eq!(config.effort, Effort::Default);
    assert!(config.cli_path.is_empty());
    assert_eq!(ProviderKind::ALL[0], ProviderKind::LocalCli);
    assert_eq!(CliTool::ALL[0], CliTool::Codex);
    let without_kind: AiConfig = serde_json::from_value(json!({"enabled": false})).unwrap();
    assert_eq!(without_kind.kind, ProviderKind::LocalCli);
    assert!(!without_kind.enabled);
}

#[test]
fn cli_resolves_an_empty_model_from_the_program_defaults() {
    let catalog = ai_cli::parse_codex_models(
        br#"{"models":[
        {"slug":"later","visibility":"list","priority":20},
        {"slug":"first","visibility":"list","priority":1}
    ]}"#,
    )
    .unwrap();
    assert_eq!(
        ai_cli::resolve_model(CliTool::Codex, "", &catalog),
        Ok("first")
    );
    assert_eq!(
        ai_cli::resolve_model(CliTool::Codex, "", &[]),
        Err("Nessun modello Codex disponibile: scegli un modello nelle Impostazioni.")
    );
    assert_eq!(
        ai_cli::resolve_model(CliTool::Claude, "", &[]),
        Ok("sonnet")
    );
    for tool in CliTool::ALL {
        assert_eq!(ai_cli::resolve_model(tool, "manual", &[]), Ok("manual"));
    }
}

#[test]
fn auto_edit_falls_back_when_the_codex_executable_is_unavailable() {
    ai_cli::with_temp_dir(|dir| {
        let config = AiConfig {
            cli_path: dir.join("missing.exe").to_string_lossy().into_owned(),
            ..AiConfig::default()
        };
        let analysis = analyze(
            Ok(AiClient::new(&config, None)),
            &[Thumbnail {
                score: 60,
                rgb: &[15; 12],
                size: (2, 2),
            }],
            true,
            |_| {},
        );
        assert!(analysis.verdict.is_none());
        let AnalysisSource::Local(reason) = analysis.source else {
            panic!("attesa l’analisi locale");
        };
        assert_eq!(
            reason,
            "Il file .exe indicato non esiste o non è accessibile."
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn old_config_defaults_to_model_effort() {
    let cfg: AiConfig = serde_json::from_value(json!({
        "enabled":true,"kind":"gemini","base_url":"https://example.com/v1","model":"vision"
    }))
    .unwrap();
    assert_eq!(cfg.effort, Effort::Default);
    let saved = serde_json::to_value(cfg).unwrap();
    assert_eq!(saved["effort"], "default");
    assert_eq!(saved["kind"], "gemini");
    assert_eq!(saved["cli_tool"], "codex");
    assert_eq!(saved["cli_path"], "");
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
        (Effort::XHigh, "high", 24576),
        (Effort::Max, "high", 24576),
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
        efforts: vec![],
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
                efforts: vec![],
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
    for kind in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
    ] {
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
            ProviderKind::LocalCli => unreachable!(),
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
    let (verdict, model) = client
        .request_verdict(&[Thumbnail {
            score: 60,
            rgb: &[15; 12],
            size: (2, 2),
        }])
        .unwrap();
    server.join().unwrap();
    assert_eq!(model, "vision");
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
