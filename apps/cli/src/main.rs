#![forbid(unsafe_code)]

use std::env;
use std::fmt::Display;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

use nexdesk_core::{DeviceId, LanEndpoint, TargetError};
use nexdesk_directory::ServerOrigin;
use nexdesk_session::{
    DesktopRequest,
    collaboration::Capabilities,
    connect_routed,
    pairing::{AccessCode, HostIdentity},
};
use zeroize::Zeroizing;

mod bridge;
mod desktop;
mod input;
mod render;
mod shell;
mod terminal;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "NexDesk CLI

Usage:
  nexdesk-cli doctor
  nexdesk-cli check-id DEVICE_ID
  nexdesk-cli check-lan IP PORT
  nexdesk-cli view DIRECTORY DEVICE_ID [--control] [--audio]
                  [--render auto|ansi|kitty|sixel] [--pin FINGERPRINT]
  nexdesk-cli shell DIRECTORY DEVICE_ID [--pin FINGERPRINT]

The password prompt has no echo. Desktop viewing starts muted.
Use Ctrl+G to enter the local command prompt and Ctrl+Q to disconnect.
";

#[tokio::main(worker_threads = 2)]
async fn main() -> ExitCode {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => show_help(),
        [help] if help == "--help" || help == "-h" => show_help(),
        [doctor] if doctor == "doctor" => {
            println!(
                "NexDesk CLI {} ({}-{})",
                env!("CARGO_PKG_VERSION"),
                env::consts::OS,
                env::consts::ARCH,
            );
            println!("Renderer: {}", render::Backend::detect().name());
            println!("Terminal input: {}", io::stdin().is_terminal());
            ExitCode::SUCCESS
        }
        [command, id] if command == "check-id" => report(id.parse::<DeviceId>()),
        [command, ip, port] if command == "check-lan" => report(LanEndpoint::parse(ip, port)),
        [command, server, device, flags @ ..]
            if matches!(command.as_str(), "view" | "shell" | "bridge") =>
        {
            match run(command, server, device, flags).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("NexDesk CLI: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

struct Target {
    origin: ServerOrigin,
    device: DeviceId,
    pin: Option<HostIdentity>,
    capabilities: Capabilities,
    input: bool,
}

impl Target {
    async fn connect(
        self,
        code: AccessCode,
        confirm: impl AsyncFnOnce(HostIdentity) -> bool,
        waiting: impl FnOnce(),
    ) -> Result<nexdesk_session::ConnectedDesktop> {
        let pin = self.pin;
        let desktop = connect_routed(
            self.origin,
            self.device,
            code,
            pin,
            DesktopRequest {
                input: self.input,
                resume: None,
                capabilities: Some(self.capabilities),
                controller: None,
                force_relay: false,
            },
            async move |identity| pin.is_some() || confirm(identity).await,
            waiting,
        )
        .await?;
        Ok(desktop)
    }
}

async fn run(command: &str, server: &str, device: &str, flags: &[String]) -> Result<()> {
    let mut target = Target {
        origin: server.parse()?,
        device: device.parse()?,
        pin: None,
        capabilities: if command == "shell" {
            Capabilities::SHELL
        } else {
            Capabilities::default()
        },
        input: false,
    };
    let mut backend = None;
    let mut flags = flags.iter().map(String::as_str);
    while let Some(flag) = flags.next() {
        match flag {
            "--pin" if target.pin.is_none() && command != "bridge" => {
                target.pin = Some(
                    flags
                        .next()
                        .ok_or_else(|| invalid("missing fingerprint"))?
                        .parse()?,
                );
            }
            "--control" if command != "shell" => target.input = true,
            "--audio" if command == "view" => {
                target.capabilities = target.capabilities.with(Capabilities::AUDIO)
            }
            "--render" if command == "view" && backend.is_none() => {
                backend = Some(
                    flags
                        .next()
                        .ok_or_else(|| invalid("missing renderer"))?
                        .parse()?,
                );
            }
            _ => return Err(invalid("unsupported option").into()),
        }
    }
    nexdesk_platform::initialize();
    if command == "bridge" {
        return bridge::run(target).await;
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(invalid("an interactive terminal is required").into());
    }
    let code = tokio::task::spawn_blocking(|| {
        let temporary = Zeroizing::new(rpassword::prompt_password("Connection password: ")?);
        AccessCode::parse(temporary.as_bytes()).map_err(|_| invalid("enter six letters or digits"))
    })
    .await??;
    let connected = target
        .connect(
            code,
            async |identity| {
                tokio::task::spawn_blocking(move || {
                    eprintln!("Host fingerprint {identity}");
                    eprintln!("Compare it with the host display and type yes to continue.");
                    let mut reply = String::new();
                    io::stdin().read_line(&mut reply).is_ok() && reply.trim() == "yes"
                })
                .await
                .unwrap_or(false)
            },
            || eprintln!("Waiting for local authorization on the host."),
        )
        .await?;
    if command == "shell" {
        shell::run(connected).await
    } else {
        desktop::run(connected, backend.unwrap_or_else(render::Backend::detect)).await
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn show_help() -> ExitCode {
    print!("{USAGE}");
    ExitCode::SUCCESS
}

fn report<T: Display>(result: std::result::Result<T, TargetError>) -> ExitCode {
    match result {
        Ok(target) => {
            println!("{target}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}
