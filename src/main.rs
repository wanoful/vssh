use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
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
use tokio::{net::TcpListener, process::Command, sync::oneshot};

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
            print!("{REMOTE_CODE_SHIM}");
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
    let remote_command = r#"mkdir -p "$HOME/.local/bin" && tmp="$(mktemp "$HOME/.local/bin/code.XXXXXX")" && cat > "$tmp" && chmod +x "$tmp" && mv "$tmp" "$HOME/.local/bin/code""#;

    let mut child = Command::new(&opts.ssh_bin)
        .args(&opts.ssh_args)
        .arg(&opts.host)
        .arg(remote_command)
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
            .write_all(REMOTE_CODE_SHIM.as_bytes())
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
        "vssh: installed remote shim at ~/.local/bin/code on {}",
        opts.host
    );
    Ok(())
}

#[derive(Clone)]
struct BridgeState {
    token: String,
    code_host: String,
    code_remote_authority: String,
    code_bin: String,
}

async fn run_bridge(
    listener: StdTcpListener,
    state: Arc<BridgeState>,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let listener = TcpListener::from_std(listener).context("failed to create tokio listener")?;
    let app = Router::new()
        .route("/open", post(open_handler))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown.await;
        })
        .await
        .context("bridge HTTP server failed")
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
    let host = if let Some(bracket_end) = host.strip_prefix('[').and_then(|host| host.find(']')) {
        &host[..bracket_end + 2]
    } else if host.matches(':').count() == 1
        && host
            .rsplit_once(':')
            .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
    {
        host.rsplit_once(':').expect("port suffix was checked").0
    } else {
        host
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };

    Ok(match user {
        Some(user) => format!("{user}@{host}:{port}"),
        None => format!("{host}:{port}"),
    })
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
"#
    );
}

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
    fn resolves_no_args_to_cwd() {
        assert_eq!(
            translate_code_args("/home/me/app", &[]).unwrap(),
            vec!["/home/me/app"]
        );
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
            "dm3:11121"
        );
        assert_eq!(
            code_remote_authority("user@dm3", &["-p2222".to_string()]).unwrap(),
            "user@dm3:2222"
        );
    }

    #[test]
    fn recognizes_ssh_port_o_option() {
        assert_eq!(
            code_remote_authority("dm3", &["-o".to_string(), "Port=11121".to_string()]).unwrap(),
            "dm3:11121"
        );
        assert_eq!(
            code_remote_authority("dm3", &["-oport=2222".to_string()]).unwrap(),
            "dm3:2222"
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
            "user@dm3:11121"
        );
        assert_eq!(
            code_remote_authority("2001:db8::1", &["-p11121".to_string()]).unwrap(),
            "[2001:db8::1]:11121"
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
