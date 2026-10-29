use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use typ_edit_collab::{Advertiser, CollabServer, ServerConfig};

const DEFAULT_PORT: u16 = 39_950;
const DEFAULT_USER: &str = "host";
const DEFAULT_MAX_CONNECTIONS: usize = 16;
const DEFAULT_READY_TIMEOUT_SECS: u64 = 30;
const READY_WAIT: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct Args {
    root: PathBuf,
    project: Option<String>,
    user: String,
    port: u16,
    password: Option<String>,
    main: Option<String>,
    image_dir: Option<PathBuf>,
    max_connections: usize,
    idle_timeout_secs: u64,
    ready_timeout_secs: u64,
    startup_delay_ms: u64,
    advertise: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!();
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = match parse_args(std::env::args_os().skip(1).collect())? {
        Some(args) => args,
        None => {
            print_usage();
            return Ok(());
        }
    };

    let root = fs::canonicalize(&args.root)
        .map_err(|error| format!("cannot open root {}: {error}", args.root.display()))?;
    if !root.is_dir() {
        return Err(format!("root {} is not a directory", root.display()));
    }
    let project = args.project.unwrap_or_else(|| {
        root.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_owned())
    });
    let main = args.main.or_else(|| {
        root.join("main.typ")
            .is_file()
            .then(|| "main.typ".to_owned())
    });
    let mut image_dir = args
        .image_dir
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                root.join(dir)
            }
        })
        .unwrap_or_else(|| root.join("image"));
    if !image_dir.starts_with(&root) {
        eprintln!(
            "warning: image dir {} is outside the root; using {}",
            image_dir.display(),
            root.join("image").display()
        );
        image_dir = root.join("image");
    }
    fs::create_dir_all(&image_dir)
        .map_err(|error| format!("cannot create image dir {}: {error}", image_dir.display()))?;

    let config = ServerConfig {
        root: root.clone(),
        project: project.clone(),
        main_file: main,
        server_user: args.user.clone(),
        port: args.port,
        image_dir,
        max_connections: args.max_connections,
        idle_timeout: (args.idle_timeout_secs > 0)
            .then(|| Duration::from_secs(args.idle_timeout_secs)),
        ready_timeout: Duration::from_secs(args.ready_timeout_secs),
        initial_docs: Vec::new(),
    };
    let needs_password = args.password.is_some();
    let server = if args.startup_delay_ms > 0 {
        CollabServer::start_with_startup_delay(
            config,
            args.password,
            Duration::from_millis(args.startup_delay_ms),
        )
        .map_err(|error| format!("server: {error}"))?
    } else {
        CollabServer::start(config, args.password).map_err(|error| format!("server: {error}"))?
    };
    if !server.wait_ready(READY_WAIT) {
        return Err("the startup scan did not finish within 300 seconds".to_owned());
    }

    let _advertiser = if args.advertise {
        match Advertiser::start(&project, &args.user, needs_password, server.port()) {
            Ok(advertiser) => Some(advertiser),
            Err(error) => {
                eprintln!("warning: mDNS advertise failed; ip:port still works: {error}");
                None
            }
        }
    } else {
        None
    };

    println!(
        "headless server: root={} project={} user={} port={} password={} advertise={}",
        root.display(),
        project,
        args.user,
        server.port(),
        if needs_password { "yes" } else { "no" },
        args.advertise
    );
    println!("HEADLESS_SERVER_READY port={}", server.port());
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // Drain the host event channel so a busy session cannot fill the bounded
    // server→host queue and stall the core. This also keeps the process alive.
    loop {
        if let Some(event) = server.recv_event_timeout(Duration::from_secs(1)) {
            println!("{event:?}");
            let _ = std::io::stdout().flush();
        }
    }
}

fn parse_args(args: Vec<std::ffi::OsString>) -> Result<Option<Args>, String> {
    let mut root: Option<PathBuf> = None;
    let mut project = None;
    let mut user = DEFAULT_USER.to_owned();
    let mut port = DEFAULT_PORT;
    let mut password = None;
    let mut main = None;
    let mut image_dir = None;
    let mut max_connections = DEFAULT_MAX_CONNECTIONS;
    let mut idle_timeout_secs = 0;
    let mut ready_timeout_secs = DEFAULT_READY_TIMEOUT_SECS;
    let mut startup_delay_ms = 0;
    let mut advertise = true;

    let mut iter = args.into_iter();
    while let Some(raw) = iter.next() {
        let flag = raw.to_string_lossy();
        match flag.as_ref() {
            "-h" | "--help" => return Ok(None),
            "--root" => root = Some(PathBuf::from(next_value(&mut iter, &flag)?)),
            "--project" => project = Some(next_string(&mut iter, &flag)?),
            "--user" => user = next_string(&mut iter, &flag)?,
            "--port" => port = parse_u16(&next_string(&mut iter, &flag)?, &flag)?,
            "--password" => password = Some(next_string(&mut iter, &flag)?),
            "--main" => main = Some(next_string(&mut iter, &flag)?),
            "--image-dir" => image_dir = Some(PathBuf::from(next_value(&mut iter, &flag)?)),
            "--max-connections" => {
                max_connections = parse_usize(&next_string(&mut iter, &flag)?, &flag)?;
            }
            "--idle-timeout-secs" => {
                idle_timeout_secs = parse_u64(&next_string(&mut iter, &flag)?, &flag)?;
            }
            "--ready-timeout-secs" => {
                ready_timeout_secs = parse_u64(&next_string(&mut iter, &flag)?, &flag)?;
            }
            "--startup-delay-ms" => {
                startup_delay_ms = parse_u64(&next_string(&mut iter, &flag)?, &flag)?;
            }
            "--advertise" => advertise = true,
            "--no-advertise" => advertise = false,
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let root = root.ok_or_else(|| "--root is required".to_owned())?;
    if user.trim().is_empty() {
        return Err("--user must not be empty".to_owned());
    }
    Ok(Some(Args {
        root,
        project,
        user,
        port,
        password,
        main,
        image_dir,
        max_connections,
        idle_timeout_secs,
        ready_timeout_secs,
        startup_delay_ms,
        advertise,
    }))
}

fn next_value(
    iter: &mut impl Iterator<Item = std::ffi::OsString>,
    flag: &str,
) -> Result<std::ffi::OsString, String> {
    iter.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn next_string(
    iter: &mut impl Iterator<Item = std::ffi::OsString>,
    flag: &str,
) -> Result<String, String> {
    next_value(iter, flag).map(|value| value.to_string_lossy().into_owned())
}

fn parse_u16(value: &str, flag: &str) -> Result<u16, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} must be a port number, got {value:?}"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} must be a non-negative integer, got {value:?}"))
}

fn parse_usize(value: &str, flag: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} must be a non-negative integer, got {value:?}"))
}

fn print_usage() {
    eprintln!(
        "UI-free Typst Editor collaboration server\n\
         \n\
         USAGE:\n  typ-edit-collab-server --root <DIR> [OPTIONS]\n\
         \n\
         OPTIONS:\n\
           --root <DIR>              project root (required)\n\
           --project <NAME>         display name (default: root directory name)\n\
           --user <NAME>            server user (default: {DEFAULT_USER})\n\
           --port <N>               first port to try (default: {DEFAULT_PORT}; 0 = ephemeral)\n\
           --password <PW>          require this password (default: none)\n\
           --main <REL>             main file (default: main.typ when present)\n\
           --image-dir <DIR>        in-root image directory (default: image)\n\
           --max-connections <N>    connection limit, 0 = unlimited (default: {DEFAULT_MAX_CONNECTIONS})\n\
           --idle-timeout-secs <N>  drop silent connections, 0 = disabled (default: 0)\n\
           --ready-timeout-secs <N> Ready deadline for joined clients (default: {DEFAULT_READY_TIMEOUT_SECS})\n\
           --startup-delay-ms <N>   debug delay before the startup scan (default: 0)\n\
           --no-advertise           do not publish an mDNS service\n\
           -h, --help               show this help\n\
         \n\
         On success prints HEADLESS_SERVER_READY port=<N> only after the startup\n\
         scan, then runs until killed."
    );
}
