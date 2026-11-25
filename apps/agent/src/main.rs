#![deny(unsafe_code)]

mod collaboration;
mod desktop_host;
mod desktop_session;
mod directory_host;
mod file_service;
mod receivers;
mod resolution;
mod server;
mod shell;

use std::{
    env,
    io::{self, BufRead},
    net::SocketAddr,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use nexdesk_core::{DEFAULT_LAN_PORT, LanEndpoint, local_listener_ip};
use nexdesk_media::{CaptureKind, CaptureTarget, Codec, MediaConfig, capture_sources};
use nexdesk_session::{
    connect,
    pairing::{AccessCode, HostIdentity},
};

const USAGE: &str = "NexDesk M1 agent

  nexdesk-agent serve [--listen IP:PORT] [--codec h264|vp8] [--fps 1..60]
                      [--screen ID | --window ID | --picker] [--browser] [--tone]
                      [--mf-h264 (Windows only)]
  nexdesk-agent sources screen|window
  nexdesk-agent probe IP PORT [--pin HOST_IDENTITY]
  nexdesk-agent host --listen IP:PORT [--screen ID | --window ID | --picker]
                     [--share-root ABSOLUTE_DIRECTORY] [--directory HTTPS_ORIGIN] [--allow-shell]

The server defaults to a synthetic test source on loopback. A screen, window
or system picker must be selected explicitly for live capture.
The probe reads one six-character code from stdin.
";

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = run().await {
        eprintln!("NexDesk agent: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    nexdesk_platform::initialize();
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, options @ ..] if command == "serve" => {
            let options = serve_options(options, false)?;
            server::run(options).await?;
        }
        [command, options @ ..] if command == "host" => {
            let options = serve_options(options, true)?;
            server::run(options).await?;
        }
        [command, kind] if command == "sources" => {
            let kind = match kind.as_str() {
                "screen" => CaptureKind::Screen,
                "window" => CaptureKind::Window,
                _ => return Err(invalid("expected screen or window").into()),
            };
            let sources = tokio::task::spawn_blocking(move || capture_sources(kind)).await??;
            println!("{}", serde_json::to_string(&sources)?);
        }
        [command] if command == "permissions" => {
            println!(
                "{}",
                serde_json::json!({"available": nexdesk_platform::request_permissions()})
            );
        }
        [command, ip, port, options @ ..] if command == "probe" => {
            let target = LanEndpoint::parse(ip, port)?;
            let pin = match options {
                [] => None,
                [flag, pin] if flag == "--pin" => Some(pin.parse::<HostIdentity>()?),
                _ => return Err(invalid("expected --pin HOST_IDENTITY").into()),
            };
            let code = std::thread::spawn(read_probe_code)
                .join()
                .map_err(|_| invalid("could not read the pairing code"))??;
            run_probe(target.socket_addr(), code, pin).await?;
        }
        [] => print!("{USAGE}"),
        [help] if help == "--help" || help == "-h" => print!("{USAGE}"),
        _ => return Err(invalid(USAGE).into()),
    }
    Ok(())
}

fn read_probe_code() -> Result<AccessCode, io::Error> {
    let mut line = [0; 8];
    let mut stdin = io::stdin().lock();
    let mut length = 0;
    while length < line.len() {
        let input = stdin.fill_buf()?;
        if input.is_empty() {
            break;
        }
        let byte = input[0];
        stdin.consume(1);
        if byte == b'\n' {
            break;
        }
        line[length] = byte;
        length += 1;
    }
    let code = line[..length]
        .strip_suffix(b"\r")
        .unwrap_or(&line[..length]);
    AccessCode::parse(code).map_err(|_| invalid("enter a six-character pairing code"))
}

async fn run_probe(
    address: SocketAddr,
    code: AccessCode,
    pin: Option<HostIdentity>,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut connection = connect(address, code, pin).await?;
    let mut frames = 0;
    let mut first_frame_ms = None;
    let mut size = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    while frames < 45 {
        match tokio::time::timeout_at(deadline, connection.view.next_frame()).await {
            Ok(Ok(Some(frame))) => {
                first_frame_ms.get_or_insert_with(|| started.elapsed().as_millis());
                size = Some((frame.width, frame.height));
                frames += 1;
            }
            Ok(Err(error)) => return Err(error.into()),
            _ => break,
        }
    }
    if frames < 45 {
        return Err(
            invalid("the probe did not receive 45 video frames before the deadline").into(),
        );
    }
    let stats = connection.view.stats().await?;
    println!(
        "{}",
        serde_json::json!({
            "identity": connection.identity.to_string(),
            "frames": frames,
            "size": size,
            "first_frame_ms": first_frame_ms,
            "elapsed_ms": started.elapsed().as_millis(),
            "media": stats,
        })
    );
    Ok(())
}

fn serve_options(args: &[String], desktop: bool) -> Result<server::Options, io::Error> {
    let mut options = server::Options {
        listen: SocketAddr::from(([127, 0, 0, 1], DEFAULT_LAN_PORT)),
        browser: false,
        desktop,
        volatile: false,
        managed: false,
        share_root: None,
        directory: None,
        allow_shell: false,
        media: MediaConfig {
            capture: CaptureTarget::Pattern,
            codec: Codec::H264,
            fps: 30,
            audio_probe: false,
            #[cfg(target_os = "windows")]
            prefer_mf_h264: false,
        },
    };
    let mut args = args.iter().map(String::as_str);
    while let Some(flag) = args.next() {
        match flag {
            "--listen" => {
                options.listen = args
                    .next()
                    .and_then(|address| address.parse().ok())
                    .ok_or_else(|| invalid("enter a literal IP and port after --listen"))?;
            }
            "--browser" => options.browser = true,
            "--volatile" if desktop => options.volatile = true,
            "--managed" if desktop => options.managed = true,
            "--allow-shell" if desktop => options.allow_shell = true,
            "--directory" if desktop => {
                options.directory = Some(
                    args.next()
                        .and_then(|value| value.parse().ok())
                        .ok_or_else(|| invalid("enter an HTTPS directory origin"))?,
                );
            }
            "--share-root" if desktop => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| invalid("choose a local shared directory"))?,
                );
                if !path.is_absolute() {
                    return Err(invalid("the shared directory must be absolute"));
                }
                options.share_root = Some(path);
            }
            "--tone" => options.media.audio_probe = true,
            #[cfg(target_os = "windows")]
            "--mf-h264" => options.media.prefer_mf_h264 = true,
            "--codec" => {
                options.media.codec = match args.next() {
                    Some("h264") => Codec::H264,
                    Some("vp8") => Codec::Vp8,
                    _ => return Err(invalid("choose h264 or vp8")),
                };
            }
            "--fps" => {
                options.media.fps = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|fps| (1..=60).contains(fps))
                    .ok_or_else(|| invalid("choose a frame rate from 1 to 60"))?;
            }
            "--screen" | "--window" => {
                let id = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| invalid("choose a capture source ID"))?;
                let kind = if flag == "--screen" {
                    CaptureKind::Screen
                } else {
                    CaptureKind::Window
                };
                set_capture(&mut options.media, CaptureTarget::Source { kind, id })?;
            }
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            "--picker" => set_capture(&mut options.media, CaptureTarget::SystemPicker)?,
            _ => return Err(invalid("unrecognized serve option")),
        }
    }
    if desktop && options.browser {
        return Err(invalid(
            "the desktop host requires native consent on each connection",
        ));
    }
    if options.allow_shell && options.directory.is_none() {
        return Err(invalid(
            "--allow-shell requires an explicit device directory",
        ));
    }
    #[cfg(target_os = "windows")]
    if options.media.prefer_mf_h264 && options.media.codec != Codec::H264 {
        return Err(invalid("--mf-h264 requires the H.264 codec"));
    }
    let ip = options.listen.ip().to_canonical();
    if !local_listener_ip(ip) || (options.browser && !ip.is_loopback()) {
        return Err(invalid(
            "M1 accepts loopback or private LAN listeners; the browser preview requires loopback",
        ));
    }
    Ok(options)
}

fn set_capture(config: &mut MediaConfig, capture: CaptureTarget) -> Result<(), io::Error> {
    if config.capture != CaptureTarget::Pattern {
        return Err(invalid("choose one capture source"));
    }
    config.capture = capture;
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
