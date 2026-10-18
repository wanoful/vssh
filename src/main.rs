use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use http_body_util::BodyExt;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use std::{
    env,
    ffi::OsString,
    net::{SocketAddr, TcpListener as StdTcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, process::Command, sync::oneshot};

const RANDOM_REMOTE_PORT_START: u16 = 40000;
const RANDOM_REMOTE_PORT_END: u16 = 60999;
const RANDOM_REMOTE_PORT_ATTEMPTS: usize = 10;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse(env::args_os().skip(1))?;

    match cli {
        Cli::Connect(opts) => connect(opts).await,
        Cli::InstallShim(opts) => install_shim(opts).await,
        Cli::PrintShim => {
            print!("{}", REMOTE_CODE_SHIM.replace("\r\n", "\n"));
            Ok(())
        }
        Cli::Help => {
            print_help();
            Ok(())
        }
    }
}

enum Cli {
    Connect(ConnectOptions),
    InstallShim(InstallShimOptions),
    PrintShim,
    Help,
}

#[derive(Debug)]
struct ConnectOptions {
    host: String,
    code_host: String,
    local_port: Option<u16>,
    remote_port: Option<u16>,
    quiet: bool,
    ssh_bin: String,
    code_bin: String,
    ssh_args: Vec<String>,
}

#[derive(Debug)]
struct InstallShimOptions {
    host: String,
    ssh_bin: String,
    ssh_args: Vec<String>,
}

impl Cli {
    fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = OsString>,
    {
        let args = args
            .into_iter()
            .map(|arg| {
                arg.into_string()
                    .map_err(|_| anyhow!("arguments must be valid UTF-8"))
            })
            .collect::<Result<Vec<_>>>()?;

        if args.is_empty() {
            return Ok(Self::Help);
        }

        match args[0].as_str() {
            "-h" | "--help" | "help" => Ok(Self::Help),
            "print-shim" => {
                ensure_no_extra(&args[1..], "print-shim")?;
                Ok(Self::PrintShim)
            }
            "install-shim" => parse_install_shim(&args[1..]).map(Self::InstallShim),
            _ => parse_connect(&args).map(Self::Connect),
        }
    }
}

fn parse_connect(args: &[String]) -> Result<ConnectOptions> {
    let mut local_port = None;
    let mut remote_port = None;
    let mut code_host = None;
    let mut quiet = false;
    let mut ssh_bin = env::var("VSSH_SSH").unwrap_or_else(|_| "ssh".to_string());
    let mut code_bin = env::var("VSSH_CODE").unwrap_or_else(|_| "code".to_string());
    let mut host = None;
    let mut ssh_args = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                ssh_args.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--local-port" => {
                i += 1;
                local_port = Some(parse_port(args.get(i), "--local-port")?);
            }
            "--remote-port" => {
                i += 1;
                remote_port = Some(parse_port(args.get(i), "--remote-port")?);
            }
            "--code-host" => {
                i += 1;
                code_host = Some(parse_value(args.get(i), "--code-host")?.to_string());
            }
            "--ssh-bin" => {
                i += 1;
                ssh_bin = parse_value(args.get(i), "--ssh-bin")?.to_string();
            }
            "--code-bin" => {
                i += 1;
                code_bin = parse_value(args.get(i), "--code-bin")?.to_string();
            }
            "-q" | "--quiet" => {
                quiet = true;
            }
            "-h" | "--help" => {
                bail!("use `vssh --help` for usage");
            }
            flag if flag.starts_with('-') => {
                bail!("unknown vssh option `{flag}`; put raw ssh arguments after `--`");
            }
            value => {
                if host.is_some() {
                    bail!("unexpected argument `{value}`; put raw ssh arguments after `--`");
                }
                host = Some(value.to_string());
            }
        }

        i += 1;
    }

    let host = host.ok_or_else(|| anyhow!("missing SSH host"))?;
    let code_host = code_host.unwrap_or_else(|| host.clone());

    Ok(ConnectOptions {
        host,
        code_host,
        local_port,
        remote_port,
        quiet,
        ssh_bin,
        code_bin,
        ssh_args,
    })
}

fn parse_install_shim(args: &[String]) -> Result<InstallShimOptions> {
    let mut ssh_bin = env::var("VSSH_SSH").unwrap_or_else(|_| "ssh".to_string());
    let mut host = None;
    let mut ssh_args = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                ssh_args.extend(args[i + 1..].iter().cloned());
                break;
            }
            "--ssh-bin" => {
                i += 1;
                ssh_bin = parse_value(args.get(i), "--ssh-bin")?.to_string();
            }
            "-h" | "--help" => {
                bail!("usage: vssh install-shim [--ssh-bin PATH] <host> [-- raw ssh args]");
            }
            flag if flag.starts_with('-') => {
                bail!("unknown install-shim option `{flag}`; put raw ssh arguments after `--`");
            }
            value => {
                if host.is_some() {
                    bail!("unexpected argument `{value}`; put raw ssh arguments after `--`");
                }
                host = Some(value.to_string());
            }
        }

        i += 1;
    }

    Ok(InstallShimOptions {
        host: host.ok_or_else(|| anyhow!("missing SSH host"))?,
        ssh_bin,
        ssh_args,
    })
}

fn ensure_no_extra(args: &[String], command: &str) -> Result<()> {
    if let Some(extra) = args.first() {
        bail!("unexpected argument `{extra}` for `{command}`");
    }
    Ok(())
}

fn parse_port(value: Option<&String>, name: &str) -> Result<u16> {
    let value = parse_value(value, name)?;
    value
        .parse()
        .with_context(|| format!("{name} must be a TCP port number"))
}

fn parse_value<'a>(value: Option<&'a String>, name: &str) -> Result<&'a str> {
    value
        .map(String::as_str)
        .ok_or_else(|| anyhow!("{name} requires a value"))
}

async fn connect(opts: ConnectOptions) -> Result<()> {
    let listener = bind_local_listener(opts.local_port)?;
    let local_addr = listener
        .local_addr()
        .context("failed to read local bridge address")?;
    let token = generate_token();
    let code_remote_authority = code_remote_authority(&opts.code_host, &opts.ssh_args)?;
    let state = Arc::new(BridgeState {
        token: token.clone(),
        code_host: opts.code_host.clone(),
        code_remote_authority,
        code_bin: opts.code_bin.clone(),
        local_dir: env::current_dir().context("failed to read local working directory")?,
        download_dir: dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
            .ok_or_else(|| anyhow!("cannot determine local Downloads directory"))?,
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let bridge = tokio::spawn(run_bridge(listener, state, shutdown_rx));

    let status = run_ssh_with_retries(&opts, local_addr, &token).await;
    let _ = shutdown_tx.send(());
    bridge
        .await
        .context("bridge task failed")?
        .context("bridge failed")?;

    let status = status?;
    if !status.success() {
        bail!("ssh exited with status {status}");
    }

    Ok(())
}

fn bind_local_listener(port: Option<u16>) -> Result<StdTcpListener> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port.unwrap_or(0)));
    let listener = StdTcpListener::bind(addr)
        .with_context(|| format!("failed to bind local bridge to {addr}"))?;
    listener
        .set_nonblocking(true)
        .context("failed to configure local bridge listener")?;
    Ok(listener)
}

fn generate_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);

    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

async fn run_ssh(
    opts: &ConnectOptions,
    local_port: u16,
    remote_port: u16,
    token: &str,
) -> Result<std::process::ExitStatus> {
    let forward = format!("127.0.0.1:{remote_port}:127.0.0.1:{local_port}");
    let remote_command = remote_shell_command(remote_port, token, &opts.code_host);

    let mut command = Command::new(&opts.ssh_bin);
    command
        .arg("-t")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-R")
        .arg(forward)
        .args(&opts.ssh_args)
        .arg(&opts.host)
        .arg(remote_command)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    command
        .status()
        .await
        .with_context(|| format!("failed to start `{}`", opts.ssh_bin))
}

async fn run_ssh_with_retries(
    opts: &ConnectOptions,
    local_addr: SocketAddr,
    token: &str,
) -> Result<std::process::ExitStatus> {
    if let Some(remote_port) = opts.remote_port {
        log_bridge(opts, local_addr, remote_port);
        return run_ssh(opts, local_addr.port(), remote_port, token).await;
    }

    let mut last_error = None;
    for attempt in 1..=RANDOM_REMOTE_PORT_ATTEMPTS {
        let remote_port = random_remote_port();
        log_bridge(opts, local_addr, remote_port);

        match run_ssh(opts, local_addr.port(), remote_port, token).await {
            Ok(status) if status.success() => return Ok(status),
            Ok(status)
                if is_retryable_ssh_status(status) && attempt < RANDOM_REMOTE_PORT_ATTEMPTS =>
            {
                last_error = Some(anyhow!("ssh exited with status {status}"));
                if !opts.quiet {
                    eprintln!(
                        "vssh: ssh exited while using remote port {remote_port}; retrying with another port"
                    );
                }
            }
            Ok(status) => return Ok(status),
            Err(error) if attempt < RANDOM_REMOTE_PORT_ATTEMPTS => {
                last_error = Some(error);
                if !opts.quiet {
                    eprintln!(
                        "vssh: failed to start ssh while using remote port {remote_port}; retrying with another port"
                    );
                }
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("failed to start ssh")))
}

fn is_retryable_ssh_status(status: std::process::ExitStatus) -> bool {
    status.code() == Some(255)
}

fn random_remote_port() -> u16 {
    let span = u32::from(RANDOM_REMOTE_PORT_END - RANDOM_REMOTE_PORT_START + 1);
    RANDOM_REMOTE_PORT_START + (OsRng.next_u32() % span) as u16
}

fn log_bridge(opts: &ConnectOptions, local_addr: SocketAddr, remote_port: u16) {
    if !opts.quiet {
        eprintln!(
            "vssh: bridge listening on {local_addr}; forwarding remote 127.0.0.1:{remote_port}"
        );
    }
}

fn remote_shell_command(remote_port: u16, token: &str, code_host: &str) -> String {
    let bridge = format!("http://127.0.0.1:{remote_port}");
    format!(
        "export LOCAL_CODE_BRIDGE={}; \
         export LOCAL_CODE_TOKEN={}; \
         export LOCAL_CODE_SSH_HOST={}; \
         export PATH=\"$HOME/.local/bin:$PATH\"; \
         exec \"${{SHELL:-/bin/sh}}\" -l",
        shell_quote(&bridge),
        shell_quote(token),
        shell_quote(code_host),
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

async fn install_shim(opts: InstallShimOptions) -> Result<()> {
    let remote_command = format!(
        "set -e; mkdir -p \"$HOME/.local/bin\"; {} {}",
        shim_install_command("code", REMOTE_CODE_SHIM),
        shim_install_command("vdown", REMOTE_DOWNLOAD_SHIM),
    );

    let mut child = Command::new(&opts.ssh_bin)
        .args(&opts.ssh_args)
        .arg(&opts.host)
        .arg("sh -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to start `{}`", opts.ssh_bin))?;

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open ssh stdin"))?;
        stdin
            .write_all(remote_command.as_bytes())
            .await
            .context("failed to send shim to ssh")?;
        stdin
            .shutdown()
            .await
            .context("failed to close ssh stdin")?;
    }

    let status = child.wait().await.context("failed to wait for ssh")?;
    if !status.success() {
        bail!("install-shim failed with status {status}");
    }

    eprintln!(
        "vssh: installed remote shims at ~/.local/bin/code and ~/.local/bin/vdown on {}",
        opts.host
    );
    Ok(())
}

fn shim_install_command(name: &str, contents: &str) -> String {
    format!(
        "tmp=$(mktemp \"$HOME/.local/bin/{name}.XXXXXX\"); trap 'rm -f \"$tmp\"' EXIT; printf %s {} > \"$tmp\"; chmod +x \"$tmp\"; mv \"$tmp\" \"$HOME/.local/bin/{name}\";",
        // include_str! preserves Windows checkout line endings, including the shebang.
        shell_quote(&contents.replace("\r\n", "\n"))
    )
}

#[derive(Clone)]
struct BridgeState {
    token: String,
    code_host: String,
    code_remote_authority: String,
    code_bin: String,
    download_dir: PathBuf,
    local_dir: PathBuf,
}

async fn run_bridge(
    listener: StdTcpListener,
    state: Arc<BridgeState>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let listener = TcpListener::from_std(listener).context("failed to create tokio listener")?;
    let app = Router::new()
        .route("/open", post(open_handler))
        .route("/download", post(download_handler))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown.await;
        })
        .await
        .context("bridge HTTP server failed")
}

#[derive(Debug, Deserialize)]
struct DownloadMetadata {
    name: String,
    destination: Option<String>,
}

async fn download_handler(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match handle_download(&state, &headers, body).await {
        Ok(path) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true, "path": path,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false, "message": format!("{error:#}"),
            })),
        )
            .into_response(),
    }
}

fn download_path(
    default_dir: &Path,
    local_dir: &Path,
    metadata: &DownloadMetadata,
) -> Result<PathBuf> {
    let name = &metadata.name;
    if name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch.is_control())
    {
        bail!("download name must be a plain filename without separators or control characters");
    }
    let directory = match metadata.destination.as_deref() {
        None => default_dir.to_path_buf(),
        Some("") => bail!("local destination directory must not be empty"),
        Some(value) if value == "~" || value.starts_with("~/") || value.starts_with("~\\") => {
            let home =
                dirs::home_dir().ok_or_else(|| anyhow!("local home directory is not set"))?;
            home.join(value.get(2..).unwrap_or(""))
        }
        Some(value) => local_dir.join(value),
    };
    Ok(directory.join(name))
}

async fn handle_download(
    state: &BridgeState,
    headers: &HeaderMap,
    mut body: Body,
) -> Result<PathBuf> {
    authorize(headers, &state.token)?;
    let metadata: DownloadMetadata = serde_json::from_str(
        headers
            .get("x-vssh-download")
            .ok_or_else(|| anyhow!("missing download metadata"))?
            .to_str()
            .context("invalid download metadata header")?,
    )
    .context("invalid download metadata")?;
    let path = download_path(&state.download_dir, &state.local_dir, &metadata)?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("missing destination directory"))?;
    tokio::fs::create_dir_all(directory)
        .await
        .with_context(|| format!("failed to create local directory {}", directory.display()))?;
    if tokio::fs::symlink_metadata(&path).await.is_ok() {
        bail!("local file already exists: {}", path.display());
    }

    // Keep an RAII temporary path so errors and cancelled requests remove partial files.
    let (file, temporary) = tempfile::Builder::new()
        .prefix(".vssh-download-")
        .tempfile_in(directory)
        .context("failed to create temporary download file")?
        .into_parts();
    let mut file = tokio::fs::File::from_std(file);
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.context("download interrupted")?.into_data() {
            file.write_all(&data)
                .await
                .context("failed to write download")?;
        }
    }
    file.flush().await.context("failed to flush download")?;
    drop(file);
    temporary.persist_noclobber(&path).with_context(|| {
        format!(
            "failed to save download (existing files are never overwritten): {}",
            path.display()
        )
    })?;
    Ok(path)
}

async fn open_handler(
    State(state): State<Arc<BridgeState>>,
    headers: HeaderMap,
    Json(request): Json<OpenRequest>,
) -> Response {
    match handle_open(&state, &headers, request).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(OpenResponse {
                ok: false,
                message: format!("{error:#}"),
                argv: Vec::new(),
            }),
        )
            .into_response(),
    }
}

async fn handle_open(
    state: &BridgeState,
    headers: &HeaderMap,
    request: OpenRequest,
) -> Result<OpenResponse> {
    authorize(headers, &state.token)?;

    if let Some(host) = &request.host
        && host != &state.code_host
    {
        bail!(
            "remote host mismatch: request used `{host}`, bridge expects `{}`",
            state.code_host
        );
    }

    let translated = translate_code_args(&request.cwd, &request.args)?;
    let mut argv = vec![
        "--remote".to_string(),
        format!("ssh-remote+{}", state.code_remote_authority),
    ];
    argv.extend(translated);

    let code_bin = resolve_local_program(&state.code_bin)?;
    let status = Command::new(&code_bin)
        .args(&argv)
        .status()
        .await
        .with_context(|| {
            format!(
                "failed to start local `{}` resolved as `{}`",
                state.code_bin,
                code_bin.display()
            )
        })?;

    if !status.success() {
        bail!("local `{}` exited with status {status}", state.code_bin);
    }

    Ok(OpenResponse {
        ok: true,
        message: "opened".to_string(),
        argv,
    })
}

fn code_remote_authority(code_host: &str, ssh_args: &[String]) -> Result<String> {
    let Some(port) = ssh_port(ssh_args)? else {
        return Ok(code_host.to_string());
    };

    let (user, host) = code_host
        .rsplit_once('@')
        .map_or((None, code_host), |(user, host)| (Some(user), host));
    let host_name =
        if let Some(bracket_end) = host.strip_prefix('[').and_then(|host| host.find(']')) {
            &host[1..bracket_end + 1]
        } else if host.matches(':').count() == 1
            && host
                .rsplit_once(':')
                .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
        {
            host.rsplit_once(':').expect("port suffix was checked").0
        } else {
            host
        };

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Authority<'a> {
        host_name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<&'a str>,
        port: u16,
    }

    let authority = serde_json::to_vec(&Authority {
        host_name,
        user,
        port,
    })
    .context("failed to encode VS Code Remote-SSH authority")?;
    Ok(hex_encode(&authority))
}

fn ssh_port(args: &[String]) -> Result<Option<u16>> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let value = if arg == "-p" {
            i += 1;
            Some(
                args.get(i)
                    .ok_or_else(|| anyhow!("SSH option `-p` requires a port"))?
                    .as_str(),
            )
        } else if let Some(value) = arg.strip_prefix("-p") {
            Some(value)
        } else if arg == "-o" {
            i += 1;
            args.get(i).and_then(|value| ssh_port_option(value))
        } else if let Some(option) = arg.strip_prefix("-o") {
            ssh_port_option(option)
        } else {
            None
        };

        if let Some(value) = value {
            return value
                .parse::<u16>()
                .with_context(|| format!("SSH port `{value}` is not a TCP port number"))
                .map(Some);
        }
        i += 1;
    }

    Ok(None)
}

fn ssh_port_option(option: &str) -> Option<&str> {
    let option = option.trim();
    let keyword_end = option
        .find(|ch: char| ch == '=' || ch.is_ascii_whitespace())
        .unwrap_or(option.len());
    if !option[..keyword_end].eq_ignore_ascii_case("port") {
        return None;
    }

    Some(option[keyword_end..].trim_start_matches(|ch: char| ch == '=' || ch.is_ascii_whitespace()))
}

fn authorize(headers: &HeaderMap, token: &str) -> Result<()> {
    let Some(value) = headers.get("authorization") else {
        bail!("missing Authorization header");
    };
    let value = value
        .to_str()
        .context("Authorization header is not valid UTF-8")?;
    let expected = format!("Bearer {token}");
    if value != expected {
        bail!("invalid Authorization token");
    }
    Ok(())
}

fn resolve_local_program(program: &str) -> Result<PathBuf> {
    let path = Path::new(program);
    if has_path_separator(program) || path.is_absolute() {
        #[cfg(windows)]
        {
            if path.extension().is_none() {
                for candidate in executable_candidates(program) {
                    let candidate = PathBuf::from(candidate);
                    if is_executable_file(&candidate) {
                        return Ok(candidate);
                    }
                }
            }
        }

        return Ok(path.to_path_buf());
    }

    let candidates = executable_candidates(program);
    let Some(path_env) = env::var_os("PATH") else {
        bail!("local `{program}` not found because PATH is not set");
    };

    for directory in env::split_paths(&path_env) {
        for candidate in &candidates {
            let path = directory.join(candidate);
            if is_executable_file(&path) {
                return Ok(path);
            }
        }
    }

    bail!(
        "local `{program}` not found in PATH; set --code-bin or VSSH_CODE to the VS Code CLI path"
    );
}

fn has_path_separator(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn executable_candidates(program: &str) -> Vec<OsString> {
    let path = Path::new(program);
    if path.extension().is_some() {
        return vec![OsString::from(program)];
    }

    #[cfg(windows)]
    {
        let mut candidates = Vec::new();
        let pathext = env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        for extension in pathext.to_string_lossy().split(';') {
            if extension.is_empty() {
                continue;
            }
            candidates.push(OsString::from(format!("{program}{extension}")));
        }
        candidates.push(OsString::from(program));
        candidates
    }

    #[cfg(not(windows))]
    {
        vec![OsString::from(program)]
    }
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(metadata) = path.metadata() else {
            return false;
        };
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    host: Option<String>,
    cwd: String,
    args: Vec<String>,
}

#[derive(Debug, Serialize)]
struct OpenResponse {
    ok: bool,
    message: String,
    argv: Vec<String>,
}

fn translate_code_args(cwd: &str, args: &[String]) -> Result<Vec<String>> {
    if cwd.is_empty() || !cwd.starts_with('/') {
        bail!("cwd must be an absolute remote path");
    }

    if args.is_empty() {
        return Ok(vec![cwd.to_string()]);
    }

    let mut translated = Vec::new();
    let mut i = 0;
    let mut paths_after_double_dash = false;

    while i < args.len() {
        let arg = &args[i];
        if paths_after_double_dash {
            translated.push(resolve_remote_path(cwd, arg));
            i += 1;
            continue;
        }

        match arg.as_str() {
            "--" => {
                translated.push(arg.clone());
                paths_after_double_dash = true;
            }
            "-r" | "--reuse-window" | "-n" | "--new-window" | "--wait" => {
                translated.push(arg.clone());
            }
            "-g" | "--goto" => {
                translated.push(arg.clone());
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow!("{arg} requires a file argument"))?;
                translated.push(resolve_goto_target(cwd, value));
            }
            "--diff" => {
                translated.push(arg.clone());
                i += 1;
                let left = args
                    .get(i)
                    .ok_or_else(|| anyhow!("--diff requires two file arguments"))?;
                translated.push(resolve_remote_path(cwd, left));
                i += 1;
                let right = args
                    .get(i)
                    .ok_or_else(|| anyhow!("--diff requires two file arguments"))?;
                translated.push(resolve_remote_path(cwd, right));
            }
            "--add" => {
                translated.push(arg.clone());
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| anyhow!("--add requires a folder argument"))?;
                translated.push(resolve_remote_path(cwd, value));
            }
            flag if flag.starts_with('-') => {
                bail!("unsupported code flag `{flag}`");
            }
            value => translated.push(resolve_remote_path(cwd, value)),
        }

        i += 1;
    }

    Ok(translated)
}

fn resolve_remote_path(cwd: &str, value: &str) -> String {
    if value == "." {
        return cwd.to_string();
    }

    if value.starts_with('/') || looks_like_uri(value) {
        return value.to_string();
    }

    normalize_remote_path(&format!("{cwd}/{value}"))
}

fn resolve_goto_target(cwd: &str, value: &str) -> String {
    let Some((path, suffix)) = split_goto_suffix(value) else {
        return resolve_remote_path(cwd, value);
    };
    format!("{}{}", resolve_remote_path(cwd, path), suffix)
}

fn split_goto_suffix(value: &str) -> Option<(&str, &str)> {
    let (path_and_line, col) = split_numeric_suffix(value)?;
    if let Some((path, line)) = split_numeric_suffix(path_and_line) {
        let suffix = &value[path.len()..];
        if !line.is_empty() && !col.is_empty() {
            return Some((path, suffix));
        }
    }

    let suffix = &value[path_and_line.len()..];
    if !col.is_empty() {
        return Some((path_and_line, suffix));
    }

    None
}

fn split_numeric_suffix(value: &str) -> Option<(&str, &str)> {
    let (left, right) = value.rsplit_once(':')?;
    if right.chars().all(|ch| ch.is_ascii_digit()) {
        Some((left, right))
    } else {
        None
    }
}

fn looks_like_uri(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };

    !scheme.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '.')
}

fn normalize_remote_path(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }

    format!("/{}", parts.join("/"))
}

fn print_help() {
    println!(
        r#"vssh 0.1.0

Usage:
  vssh [options] <host> [-- raw ssh args]
  vssh install-shim [options] <host> [-- raw ssh args]
  vssh print-shim

Connect options:
  --code-host HOST     VS Code Remote-SSH target name. Defaults to <host>.
  --local-port PORT    Local bridge port. Defaults to an ephemeral port.
  --remote-port PORT   Remote loopback port for reverse forwarding. Defaults to a random high port.
  -q, --quiet          Suppress bridge startup logging.
  --ssh-bin PATH       SSH executable. Defaults to $VSSH_SSH or ssh.
  --code-bin PATH      Local VS Code CLI. Defaults to $VSSH_CODE or code.

Examples:
  vssh install-shim devbox
  vssh devbox
  vssh --code-host devbox-alias user@example.com

Inside the remote shell started by vssh:
  code .
  code file.rs
  code -g src/main.rs:42:1
  code -r .
  vdown file1 file2
  vdown file1 file2 -d '~/Downloads/project'
  vdown file1 file2 -- /local/path
"#
    );
}

const REMOTE_DOWNLOAD_SHIM: &str = include_str!("vdown.py");

const REMOTE_CODE_SHIM: &str = r#"#!/usr/bin/env python3
import json
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.request


def find_real_code():
    self_path = os.path.realpath(sys.argv[0])
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        if not directory:
            continue
        candidate = os.path.join(directory, "code")
        if os.path.realpath(candidate) == self_path:
            continue
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def fallback():
    real_code = find_real_code()
    if real_code:
        os.execv(real_code, [real_code] + sys.argv[1:])
    print(
        "vssh: LOCAL_CODE_BRIDGE is not set. Start this shell with `vssh <host>`.",
        file=sys.stderr,
    )
    return 127


def main():
    bridge = os.environ.get("LOCAL_CODE_BRIDGE")
    token = os.environ.get("LOCAL_CODE_TOKEN")
    host = os.environ.get("LOCAL_CODE_SSH_HOST")
    if not bridge or not token or not host:
        return fallback()

    payload = {
        "host": host,
        "cwd": os.getcwd(),
        "args": sys.argv[1:],
    }
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        bridge.rstrip("/") + "/open",
        data=body,
        method="POST",
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )

    try:
        with urllib.request.urlopen(request, timeout=None) as response:
            response.read()
        return 0
    except urllib.error.HTTPError as error:
        message = error.read().decode("utf-8", errors="replace")
        print("vssh: local code bridge rejected the request:", message, file=sys.stderr)
        return 1
    except Exception as error:
        print("vssh: failed to reach local code bridge: {}".format(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_windows_shim_line_endings() {
        let lf = "#!/usr/bin/env python3\nprint('hello')\n";
        assert_eq!(
            shim_install_command("vdown", &lf.replace('\n', "\r\n")),
            shim_install_command("vdown", lf)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installs_both_shims() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let fake_ssh = directory.path().join("ssh");
        // Run the install script in an isolated home, as the remote sh would.
        std::fs::write(
            &fake_ssh,
            format!(
                "#!/bin/sh\nexport HOME={}\nexec sh -s\n",
                shell_quote(directory.path().to_str().unwrap())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        install_shim(InstallShimOptions {
            host: "test".into(),
            ssh_bin: fake_ssh.to_str().unwrap().into(),
            ssh_args: vec![],
        })
        .await
        .unwrap();
        // Simulate shims embedded from a Windows CRLF checkout and reinstall them.
        let script = format!(
            "set -e; {} {}",
            shim_install_command("code", &REMOTE_CODE_SHIM.replace('\n', "\r\n")),
            shim_install_command("vdown", &REMOTE_DOWNLOAD_SHIM.replace('\n', "\r\n")),
        );
        assert!(!script.contains('\r'));
        let mut child = Command::new(&fake_ssh)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(script.as_bytes()).await.unwrap();
        drop(stdin);
        assert!(child.wait().await.unwrap().success());
        for (name, contents) in [("code", REMOTE_CODE_SHIM), ("vdown", REMOTE_DOWNLOAD_SHIM)] {
            let path = directory.path().join(".local/bin").join(name);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                contents.replace("\r\n", "\n")
            );
            assert!(is_executable_file(&path));
        }
        let output = Command::new(directory.path().join(".local/bin/vdown"))
            .arg("--help")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_dir(directory.path().join(".local/bin"))
                .unwrap()
                .count(),
            2
        );
    }

    fn download_state(directory: &Path) -> BridgeState {
        BridgeState {
            token: "test-token".into(),
            code_host: "test".into(),
            code_remote_authority: "test".into(),
            code_bin: "code".into(),
            download_dir: directory.join("Downloads"),
            local_dir: directory.to_path_buf(),
        }
    }

    #[test]
    fn download_destinations_and_names() {
        let state = download_state(Path::new("/local"));
        let mut metadata = DownloadMetadata {
            name: "file.txt".into(),
            destination: None,
        };
        assert_eq!(
            download_path(&state.download_dir, &state.local_dir, &metadata).unwrap(),
            Path::new("/local/Downloads/file.txt")
        );
        metadata.destination = Some("custom".into());
        assert_eq!(
            download_path(&state.download_dir, &state.local_dir, &metadata).unwrap(),
            Path::new("/local/custom/file.txt")
        );
        for name in ["", ".", "..", "../escape", "a/b", "a\\b", "C:stream", "a\n"] {
            metadata.name = name.into();
            assert!(download_path(&state.download_dir, &state.local_dir, &metadata).is_err());
        }
    }

    #[tokio::test]
    async fn download_requires_auth_and_refuses_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let state = download_state(directory.path());
        let mut headers = HeaderMap::new();
        headers.insert("x-vssh-download", r#"{"name":"test.bin"}"#.parse().unwrap());
        assert!(
            handle_download(&state, &headers, Body::empty())
                .await
                .is_err()
        );
        assert!(!state.download_dir.exists());
        headers.insert("authorization", "Bearer test-token".parse().unwrap());
        let path = handle_download(&state, &headers, Body::from("original"))
            .await
            .unwrap();
        assert!(
            handle_download(&state, &headers, Body::from("replacement"))
                .await
                .is_err()
        );
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&state.download_dir).unwrap().count(), 1);
    }

    #[test]
    fn resolves_no_args_to_cwd() {
        assert_eq!(
            translate_code_args("/home/me/app", &[]).unwrap(),
            vec!["/home/me/app"]
        );
    }

    #[tokio::test]
    async fn python_shim_streams_files_to_bridge() {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(download_state(directory.path()));
        let listener = bind_local_listener(None).unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let bridge = tokio::spawn(run_bridge(listener, state.clone(), shutdown_rx));
        let source = directory.path().join("远程 file ' $.bin");
        let content: Vec<u8> = (0..3_000_000).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&source, &content).await.unwrap();
        let empty = directory.path().join("empty");
        tokio::fs::write(&empty, []).await.unwrap();
        let shim = directory.path().join("vdown.py");
        tokio::fs::write(&shim, REMOTE_DOWNLOAD_SHIM).await.unwrap();
        let invoke = |arguments: Vec<OsString>| {
            let mut command = Command::new("python3");
            command
                .arg(&shim)
                .args(arguments)
                .env("LOCAL_CODE_BRIDGE", format!("http://{address}"))
                .env("LOCAL_CODE_TOKEN", &state.token)
                .env("http_proxy", "http://127.0.0.1:1");
            command
        };
        let output = invoke(vec![source.clone().into(), empty.clone().into()])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            tokio::fs::read(state.download_dir.join(source.file_name().unwrap()))
                .await
                .unwrap(),
            content
        );
        assert_eq!(
            tokio::fs::read(state.download_dir.join("empty"))
                .await
                .unwrap(),
            b""
        );
        let output = invoke(vec![empty.clone().into()]).output().await.unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
        for flag in ["-d", "--"] {
            let destination = directory.path().join(format!("custom-{flag}"));
            let output = invoke(vec![
                empty.clone().into(),
                flag.into(),
                destination.clone().into(),
            ])
            .output()
            .await
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(destination.join("empty").is_file());
        }
        // A truncated HTTP body must leave neither a final file nor a partial file.
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket.write_all(b"POST /download HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer test-token\r\nX-Vssh-Download: {\"name\":\"broken\"}\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort").await.unwrap();
        socket.shutdown().await.unwrap();
        let mut response = Vec::new();
        use tokio::io::AsyncReadExt;
        socket.read_to_end(&mut response).await.unwrap();
        let _ = shutdown_tx.send(());
        bridge.await.unwrap().unwrap();
        assert_eq!(std::fs::read_dir(&state.download_dir).unwrap().count(), 2);
    }

    #[test]
    fn resolves_relative_paths() {
        assert_eq!(
            translate_code_args("/home/me/app", &["src/main.rs".to_string()]).unwrap(),
            vec!["/home/me/app/src/main.rs"]
        );
    }

    #[test]
    fn resolves_goto_paths() {
        assert_eq!(
            translate_code_args(
                "/home/me/app",
                &["-g".to_string(), "src/main.rs:12:4".to_string()]
            )
            .unwrap(),
            vec!["-g", "/home/me/app/src/main.rs:12:4"]
        );
    }

    #[test]
    fn preserves_allowed_flags() {
        assert_eq!(
            translate_code_args(
                "/home/me/app",
                &["-r".to_string(), "--wait".to_string(), ".".to_string()]
            )
            .unwrap(),
            vec!["-r", "--wait", "/home/me/app"]
        );
    }

    #[test]
    fn rejects_unknown_flags() {
        assert!(translate_code_args("/home/me/app", &["--install-extension".to_string()]).is_err());
    }

    #[test]
    fn normalizes_dot_dot() {
        assert_eq!(
            resolve_remote_path("/home/me/app/src", "../README.md"),
            "/home/me/app/README.md"
        );
    }

    #[test]
    fn quotes_shell_values() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn adds_raw_ssh_port_to_code_authority() {
        assert_eq!(
            code_remote_authority("dm3", &["-p".to_string(), "11121".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22646d33222c22706f7274223a31313132317d"
        );
        assert_eq!(
            code_remote_authority("user@dm3", &["-p2222".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22646d33222c2275736572223a2275736572222c22706f7274223a323232327d"
        );
    }

    #[test]
    fn recognizes_ssh_port_o_option() {
        assert_eq!(
            code_remote_authority("dm3", &["-o".to_string(), "Port=11121".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22646d33222c22706f7274223a31313132317d"
        );
        assert_eq!(
            code_remote_authority("dm3", &["-oport=2222".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22646d33222c22706f7274223a323232327d"
        );
    }

    #[test]
    fn keeps_code_authority_without_raw_ssh_port() {
        assert_eq!(
            code_remote_authority("dm3", &["-i".to_string(), "key".to_string()]).unwrap(),
            "dm3"
        );
    }

    #[test]
    fn replaces_existing_code_authority_port() {
        assert_eq!(
            code_remote_authority("user@dm3:22", &["-p11121".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22646d33222c2275736572223a2275736572222c22706f7274223a31313132317d"
        );
        assert_eq!(
            code_remote_authority("2001:db8::1", &["-p11121".to_string()]).unwrap(),
            "7b22686f73744e616d65223a22323030313a6462383a3a31222c22706f7274223a31313132317d"
        );
    }

    #[test]
    fn keeps_explicit_program_extension() {
        assert_eq!(
            executable_candidates("code.cmd"),
            vec![OsString::from("code.cmd")]
        );
    }

    #[test]
    fn random_remote_port_stays_in_range() {
        for _ in 0..100 {
            let port = random_remote_port();
            assert!((RANDOM_REMOTE_PORT_START..=RANDOM_REMOTE_PORT_END).contains(&port));
        }
    }
}
