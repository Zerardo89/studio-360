//! Discovery and supervised execution of installed vision CLIs on Windows.

use std::{
    collections::{HashSet, VecDeque},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{self, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use windows::{
    core::{HSTRING, PCWSTR},
    Management::Deployment::PackageManager,
    Win32::{
        Foundation::{HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{WaitForSingleObject, CREATE_NO_WINDOW},
            WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        },
    },
};

use crate::ai_provider::{base64, AiConfig, Effort, ModelInfo};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const CATALOG_LIMIT: usize = 8 * 1024 * 1024;
const ERROR_LIMIT: usize = 300;
const TOKEN_LIMIT: usize = 100;
const OUTPUT_FILE: &str = "out.txt";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliTool {
    #[default]
    Claude,
    Codex,
}

impl CliTool {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude.exe",
            Self::Codex => "codex.exe",
        }
    }
}

pub(crate) fn validate_config(config: &AiConfig) -> Result<(), String> {
    validate_arguments(&config.model, None)?;
    if !config.cli_path.is_empty() && !has_exe_extension(Path::new(&config.cli_path)) {
        return Err("Il percorso del programma AI deve indicare un file .exe.".into());
    }
    Ok(())
}

pub(crate) fn validated_executable(config: &AiConfig) -> Result<PathBuf, String> {
    validate_config(config)?;
    resolve_executable(config.cli_tool, &config.cli_path)
}

fn has_exe_extension(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

pub fn resolve_executable(tool: CliTool, configured_path: &str) -> Result<PathBuf, String> {
    if !configured_path.is_empty() {
        let path =
            path::absolute(configured_path).map_err(|_| "Percorso del programma AI non valido.")?;
        if !has_exe_extension(&path) {
            return Err("Il percorso del programma AI deve indicare un file .exe.".into());
        }
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| "Il file .exe indicato non esiste o non è accessibile.".into());
    }

    let name = tool.executable();
    if let Some(path) = env::var_os("PATH").and_then(|value| {
        env::split_paths(&value)
            .filter(|directory| !directory.as_os_str().is_empty())
            .filter_map(|directory| path::absolute(directory.join(name)).ok())
            .find(|path| path.is_file())
    }) {
        return Ok(path);
    }
    let installed = match tool {
        CliTool::Claude => env::var_os("USERPROFILE")
            .and_then(|profile| {
                path::absolute(PathBuf::from(profile).join(r".local\bin\claude.exe")).ok()
            })
            .filter(|path| path.is_file()),
        CliTool::Codex => store_codex(),
    };
    installed.ok_or_else(|| {
        format!(
            "{} non trovato: installa il programma o indica il percorso.",
            tool.label()
        )
    })
}

fn store_codex() -> Option<PathBuf> {
    struct Runtime(bool);
    impl Drop for Runtime {
        fn drop(&mut self) {
            if self.0 {
                // This guard balances the successful RoInitialize on this thread.
                unsafe { RoUninitialize() };
            }
        }
    }

    let _runtime = match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
        Ok(()) => Runtime(true),
        Err(error) if error.code() == RPC_E_CHANGED_MODE => Runtime(false),
        Err(_) => return None,
    };
    let packages = PackageManager::new()
        .ok()?
        .FindPackagesByUserSecurityIdPackageFamilyName(
            &HSTRING::new(),
            &HSTRING::from("OpenAI.Codex_2p2nqsd0c76g0"),
        )
        .ok()?;
    for package in packages {
        if let Ok(location) = package.InstalledLocation().and_then(|folder| folder.Path()) {
            let executable =
                PathBuf::from(location.to_os_string()).join(r"app\resources\codex.exe");
            if executable.is_file() {
                return Some(executable);
            }
        }
    }
    None
}

fn valid_argument(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= TOKEN_LIMIT
        && !token.starts_with('-')
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

fn validate_arguments(model: &str, effort: Option<&str>) -> Result<(), String> {
    if model.is_empty() {
        return Err("modello AI mancante".into());
    }
    if !valid_argument(model) {
        return Err("ID del modello non valido. Usa fino a 100 caratteri tra A–Z, a–z, 0–9 e ._:-, senza iniziare con un trattino.".into());
    }
    if effort.is_some_and(|value| {
        !valid_argument(value)
            || !Effort::EXPLICIT
                .iter()
                .any(|level| level.cli_level() == Some(value))
    }) {
        return Err("livello di ragionamento del programma AI non valido".into());
    }
    Ok(())
}

pub(crate) fn claude_args<'a>(
    model: &'a str,
    effort: Option<&'a str>,
) -> Result<Vec<&'a str>, String> {
    validate_arguments(model, effort)?;
    let mut args = vec![
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
        model,
    ];
    if let Some(effort) = effort {
        args.extend(["--effort", effort]);
    }
    Ok(args)
}

pub(crate) fn codex_args(
    directory: &Path,
    model: &str,
    effort: Option<&str>,
    images: &[PathBuf],
) -> Result<Vec<OsString>, String> {
    validate_arguments(model, effort)?;
    let mut args: Vec<OsString> = [
        "exec",
        "--skip-git-repo-check",
        "--ephemeral",
        "-s",
        "read-only",
        "--ignore-user-config",
        "--color",
        "never",
        "-C",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend([directory.as_os_str(), OsStr::new("-m"), OsStr::new(model)].map(OsString::from));
    if let Some(effort) = effort {
        args.extend([
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\"").into(),
        ]);
    }
    for image in images {
        args.extend(["-i".into(), image.as_os_str().to_owned()]);
    }
    args.extend([
        "-o".into(),
        directory.join(OUTPUT_FILE).into_os_string(),
        "-".into(),
    ]);
    Ok(args)
}

pub(crate) fn claude_input(prompt: &str, images: &[Vec<u8>]) -> Vec<u8> {
    let mut content: Vec<_> = images.iter().map(|jpeg| {
        json!({"type":"image", "source":{"type":"base64", "media_type":"image/jpeg", "data":base64(jpeg)}})
    }).collect();
    content.push(json!({"type":"text", "text":prompt}));
    let mut line = json!({"type":"user", "message":{"role":"user", "content":content}})
        .to_string()
        .into_bytes();
    line.push(b'\n');
    line
}

pub(crate) fn parse_claude_result(stdout: &[u8], stderr: &[u8]) -> Result<String, String> {
    let result = stdout
        .rsplit(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .find(|event| event["type"] == "result");
    if let Some(result) = result {
        if let Some(text) = result["result"]
            .as_str()
            .filter(|text| !text.trim().is_empty())
        {
            if result["is_error"] != true {
                return Ok(text.to_owned());
            }
            let detail = error_tail(text.as_bytes());
            if !detail.is_empty() {
                return Err(detail);
            }
        }
    }
    Err(stderr_or(
        stderr,
        "Claude Code non ha restituito un risultato.",
    ))
}

pub fn list_models(config: &AiConfig) -> Result<Vec<ModelInfo>, String> {
    match config.cli_tool {
        CliTool::Claude => Ok([
            ("fable", "Fable (più recente)"),
            ("opus", "Opus (più recente)"),
            ("sonnet", "Sonnet (più recente)"),
            ("haiku", "Haiku (più recente)"),
        ]
        .into_iter()
        .map(|(id, label)| ModelInfo {
            id: id.into(),
            label: label.into(),
            efforts: Effort::EXPLICIT.to_vec(),
        })
        .collect()),
        CliTool::Codex => codex_models(&resolve_executable(config.cli_tool, &config.cli_path)?),
    }
}

fn codex_models(executable: &Path) -> Result<Vec<ModelInfo>, String> {
    with_temp_dir(|dir| {
        let output = run_process(
            executable,
            &["debug", "models"],
            dir,
            Vec::new(),
            RunLimits::new(CATALOG_TIMEOUT, CATALOG_LIMIT),
        )?;
        parse_codex_models(output.stdout.complete()?)
    })
}

pub(crate) fn parse_codex_models(stdout: &[u8]) -> Result<Vec<ModelInfo>, String> {
    #[derive(Deserialize)]
    struct Catalog {
        models: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        slug: String,
        #[serde(default)]
        display_name: String,
        visibility: String,
        #[serde(default = "last_priority")]
        priority: i64,
        #[serde(default)]
        supported_reasoning_levels: Vec<Level>,
    }
    #[derive(Deserialize)]
    struct Level {
        effort: String,
    }
    fn last_priority() -> i64 {
        i64::MAX
    }

    let mut catalog: Catalog = serde_json::from_slice(stdout)
        .map_err(|_| "Il catalogo dei modelli di Codex non è valido.")?;
    catalog
        .models
        .retain(|entry| entry.visibility == "list" && valid_argument(&entry.slug));
    catalog.models.sort_by_key(|entry| entry.priority);
    let mut seen = HashSet::new();
    Ok(catalog
        .models
        .into_iter()
        .filter_map(|entry| {
            if !seen.insert(entry.slug.clone()) {
                return None;
            }
            let efforts = Effort::EXPLICIT
                .into_iter()
                .filter(|value| {
                    entry
                        .supported_reasoning_levels
                        .iter()
                        .any(|level| value.cli_level() == Some(level.effort.as_str()))
                })
                .collect();
            let label = entry.display_name.trim();
            let label = if label.is_empty() || label.chars().any(char::is_control) {
                entry.slug.clone()
            } else {
                label.to_owned()
            };
            Some(ModelInfo {
                id: entry.slug,
                label,
                efforts,
            })
        })
        .collect())
}

pub fn send(
    config: &AiConfig,
    catalog_effort: Option<Effort>,
    prompt: &str,
    images: &[Vec<u8>],
) -> Result<String, String> {
    let limits = RunLimits::new(REQUEST_TIMEOUT, OUTPUT_LIMIT);
    let executable = validated_executable(config)?;
    let effort = catalog_effort
        .unwrap_or_else(|| {
            // Auto Edit can start before the settings catalog has been loaded.
            let models = if config.cli_tool == CliTool::Codex && config.effort != Effort::Default {
                codex_models(&executable).unwrap_or_default()
            } else {
                Vec::new()
            };
            config.effective_effort(&models)
        })
        .cli_level();
    with_temp_dir(|dir| match config.cli_tool {
        CliTool::Claude => {
            let args = claude_args(&config.model, effort)?;
            let output = run_process(
                &executable,
                &args,
                dir,
                claude_input(prompt, images),
                limits,
            )?;
            parse_claude_result(output.stdout.complete()?, &output.stderr.data)
        }
        CliTool::Codex => {
            let images = images
                .iter()
                .enumerate()
                .map(|(index, jpeg)| {
                    let path = dir.join(format!("img-{:02}.jpg", index + 1));
                    fs::write(&path, jpeg)
                        .map_err(|_| "impossibile scrivere una miniatura JPEG temporanea")?;
                    Ok(path)
                })
                .collect::<Result<Vec<_>, &str>>()?;
            let args = codex_args(dir, &config.model, effort, &images)?;
            let output = run_process(&executable, &args, dir, prompt.as_bytes().to_vec(), limits)?;
            read_codex_result(dir, &output.stderr.data)
        }
    })
}

fn read_codex_result(directory: &Path, stderr: &[u8]) -> Result<String, String> {
    let file = fs::File::open(directory.join(OUTPUT_FILE))
        .map_err(|_| stderr_or(stderr, "Codex non ha scritto il file della risposta."))?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "impossibile leggere il file della risposta di Codex")?;
    if bytes.len() > OUTPUT_LIMIT {
        return Err("la risposta di Codex supera il limite di 2 MiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "risposta di Codex non valida")?;
    if text.trim().is_empty() {
        return Err(stderr_or(stderr, "Codex ha risposto senza testo."));
    }
    Ok(text)
}

pub(crate) fn with_temp_dir<T>(work: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    let root = path::absolute(env::temp_dir()).map_err(|_| "cartella temporanea non valida")?;
    let directory = loop {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("Studio360-ai-{}-{sequence}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => break TempDir(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                return Err("impossibile creare la cartella temporanea del programma AI".into())
            }
        }
    };
    work(&directory.0)
}

struct Captured {
    data: Vec<u8>,
    truncated: bool,
}

impl Captured {
    fn complete(&self) -> Result<&[u8], String> {
        if self.truncated {
            Err("l’output del programma AI supera il limite previsto".into())
        } else {
            Ok(&self.data)
        }
    }
}

pub(crate) struct ProcessOutput {
    stdout: Captured,
    stderr: Captured,
}

fn capture(mut pipe: impl Read, limit: usize, keep_tail: bool) -> io::Result<Captured> {
    let mut data = VecDeque::new();
    let mut truncated = false;
    let mut chunk = [0; 8192];
    // Keep draining after reaching the limit so a full pipe cannot block the process.
    loop {
        let read = pipe.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let room = limit - data.len();
        truncated |= read > room;
        if keep_tail && read > room {
            data.drain(..(read - room).min(data.len()));
        }
        let bytes = if keep_tail {
            &chunk[read.saturating_sub(limit)..read]
        } else {
            &chunk[..read.min(room)]
        };
        data.extend(bytes);
    }
    Ok(Captured {
        data: data.into(),
        truncated,
    })
}

fn error_tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut tail: VecDeque<_> = VecDeque::with_capacity(ERROR_LIMIT);
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !tail.is_empty();
            continue;
        }
        if character.is_control() {
            continue;
        }
        if pending_space {
            if tail.len() == ERROR_LIMIT {
                tail.pop_front();
            }
            tail.push_back(' ');
            pending_space = false;
        }
        if tail.len() == ERROR_LIMIT {
            tail.pop_front();
        }
        tail.push_back(character);
    }
    tail.into_iter().collect::<String>().trim_start().to_owned()
}

fn stderr_or(stderr: &[u8], fallback: &str) -> String {
    let tail = error_tail(stderr);
    if tail.is_empty() {
        fallback.to_owned()
    } else {
        tail
    }
}

fn process_job() -> windows::core::Result<OwnedHandle> {
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // Own the handle before setting limits so an error also closes it.
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())?;
        let owned = OwnedHandle::from_raw_handle(job.0);
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;
        Ok(owned)
    }
}

pub(crate) struct RunLimits {
    deadline: Instant,
    timeout: Duration,
    output: usize,
}

impl RunLimits {
    pub(crate) fn new(timeout: Duration, output: usize) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            timeout,
            output,
        }
    }

    fn timeout_error(&self) -> String {
        format!(
            "il programma AI non ha risposto entro {} s",
            self.timeout.as_secs()
        )
    }
}

pub(crate) fn run_process(
    executable: &Path,
    args: &[impl AsRef<OsStr>],
    directory: &Path,
    input: Vec<u8>,
    limits: RunLimits,
) -> Result<ProcessOutput, String> {
    if Instant::now() >= limits.deadline {
        return Err(limits.timeout_error());
    }
    let job = process_job().map_err(|error| {
        format!(
            "impossibile creare il Job Object del programma AI ({:#x})",
            error.code().0
        )
    })?;
    let mut child = Command::new(executable)
        .args(args)
        .current_dir(directory)
        .creation_flags(CREATE_NO_WINDOW.0)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("impossibile avviare il programma AI: {error}"))?;
    let process = HANDLE(child.as_raw_handle());
    if let Err(error) = unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), process) } {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "impossibile assegnare il programma AI al Job Object ({:#x})",
            error.code().0
        ));
    }
    let mut stdin = child.stdin.take().expect("stdin del processo");
    let stdout = child.stdout.take().expect("stdout del processo");
    let stderr = child.stderr.take().expect("stderr del processo");
    let limit = limits.output;
    let stdout_reader = thread::spawn(move || capture(stdout, limit, false));
    let stderr_reader = thread::spawn(move || capture(stderr, limit, true));
    let writer = thread::spawn(move || stdin.write_all(&input));

    let remaining = limits.deadline.saturating_duration_since(Instant::now());
    let millis = remaining.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    let wait = unsafe { WaitForSingleObject(process, millis) };
    // Closing the last handle stops the entire job, including any child holding a pipe open.
    drop(job);
    let status = child.wait();
    let written = writer.join();
    let stdout = stdout_reader.join();
    let stderr = stderr_reader.join();
    if wait == WAIT_TIMEOUT {
        return Err(limits.timeout_error());
    }
    if wait != WAIT_OBJECT_0 {
        return Err("impossibile attendere la fine del programma AI".into());
    }
    let status = status.map_err(|_| "impossibile leggere il codice di uscita del programma AI")?;
    let stderr = stderr
        .ok()
        .and_then(Result::ok)
        .ok_or("impossibile leggere gli errori del programma AI")?;
    if !status.success() {
        return Err(stderr_or(
            &stderr.data,
            &format!("il programma AI è terminato con {status}"),
        ));
    }
    written
        .ok()
        .and_then(Result::ok)
        .ok_or("impossibile inviare la richiesta al programma AI")?;
    let stdout = stdout
        .ok()
        .and_then(Result::ok)
        .ok_or("impossibile leggere l’output del programma AI")?;
    Ok(ProcessOutput { stdout, stderr })
}
