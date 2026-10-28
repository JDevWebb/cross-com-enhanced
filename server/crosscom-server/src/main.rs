//! The Cross-Com server for Ghost Recon: Future Soldier.
//!
//! Three services, each on its own thread:
//! - the online config, over HTTP, which tells the game where to sign in;
//! - the auth server, where the game signs in and gets a ticket for the secure server;
//! - the secure server, where the game's online services are.
//!
//! ```text
//! crosscom-server [--config crosscom-server.toml]
//! crosscom-server [--config crosscom-server.toml] add-account NAME PASSWORD
//! ```

mod accounts;
mod config;
mod fixed;
mod game_session;
mod menu;
mod online_config;
mod privileges;
mod rate_limit;
mod secure;
mod sessions;
mod stats;
mod ticket;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use quazal::prudp::packet::QPacket;
use quazal::prudp::packet::StreamHandlerRegistry;
use quazal::prudp::packet::StreamType;
use quazal::prudp::packet::VPort;
use quazal::prudp::Server;
use quazal::rmc::RVSecHandler;
use quazal::Context;
use slog::error;
use slog::info;
use slog::Logger;
use sloggers::terminal::Destination;
use sloggers::terminal::TerminalLoggerBuilder;
use sloggers::types::Severity;
use sloggers::Build;
use sodiumoxide::crypto::secretbox;

use crate::accounts::Accounts;
use crate::config::Config;

enum Command {
    Serve,
    AddAccount { name: String, password: String },
}

fn parse_args() -> Result<(PathBuf, Command), String> {
    let mut config = PathBuf::from("crosscom-server.toml");
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            config = args.next().ok_or("--config needs a file")?.into();
        } else {
            rest.push(arg);
        }
    }
    let command = match rest.as_slice() {
        [] => Command::Serve,
        [cmd, name, password] if cmd == "add-account" => Command::AddAccount {
            name: name.clone(),
            password: password.clone(),
        },
        _ => return Err("usage: crosscom-server [--config FILE] [add-account NAME PASSWORD]".into()),
    };
    Ok((config, command))
}

fn logger(level: &str) -> Logger {
    let severity = level.parse().unwrap_or(Severity::Info);
    TerminalLoggerBuilder::new()
        .level(severity)
        .destination(Destination::Stderr)
        .build()
        .expect("a terminal logger")
}

fn main() -> ExitCode {
    let (config_path, command) = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let accounts = match Accounts::open(&config.database) {
        Ok(accounts) => Arc::new(accounts),
        Err(e) => {
            eprintln!("couldn't open {}: {e}", config.database.display());
            return ExitCode::FAILURE;
        }
    };

    match command {
        Command::AddAccount { name, password } => match accounts.add(&name, &password) {
            Ok(pid) => {
                println!("Created {name} (PID {pid:#x})");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("couldn't create {name}: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Serve => serve(&config, accounts),
    }
}

fn serve(config: &Config, accounts: Arc<Accounts>) -> ExitCode {
    let logger = logger(&config.log_level);
    if sodiumoxide::init().is_err() {
        error!(logger, "couldn't start libsodium");
        return ExitCode::FAILURE;
    }
    let public_ip = match config.public_ip() {
        Ok(ip) => ip,
        Err(e) => {
            error!(logger, "{e}");
            return ExitCode::FAILURE;
        }
    };
    info!(logger, "Cross-Com server {}; players connect to {public_ip}", env!("CARGO_PKG_VERSION"));

    // Both servers share the key that seals tickets: the auth server issues them, the secure
    // server opens them.
    let ticket_key = secretbox::gen_key();
    let context = |port: u16| Context {
        listen: SocketAddr::new(config.listen_ip, port),
        secure_server_addr: Some(SocketAddr::new(public_ip, config.secure_port)),
        ticket_key: ticket_key.clone(),
        ..Context::ghost_recon_future_soldier()
    };
    let auth = context(config.auth_port);
    let secure = context(config.secure_port);
    let http_listen = SocketAddr::new(config.listen_ip, config.http_port);
    let online_config = online_config::content(public_ip, config.auth_port);

    let threads = [
        spawn("online config", logger.clone(), move |logger| online_config::serve(&logger, http_listen, online_config)),
        spawn("auth", logger.clone(), {
            let accounts = Arc::clone(&accounts);
            move |logger| run_server(&logger, &auth, |handler| handler.register_protocol(ticket::new_protocol(accounts)))
        }),
        spawn("secure", logger.clone(), move |logger| {
            let sessions = Arc::new(sessions::Sessions::default());
            run_server(&logger, &secure, |handler| {
                handler.register_protocol(secure::new_protocol(Arc::clone(&sessions)));
                handler.register_protocol(game_session::new_protocol(Arc::clone(&sessions), Arc::clone(&accounts)));
                handler.register_protocol(privileges::new_protocol());
                for protocol in menu::protocols().into_iter().chain(fixed::protocols()) {
                    handler.register_protocol(protocol);
                }
                handler.register_protocol(Box::new(stats::PlayerStatistics));
            })
        }),
    ];
    for thread in threads {
        let _ = thread.join();
    }
    ExitCode::FAILURE
}

fn spawn<F>(name: &'static str, logger: Logger, f: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce(Logger) -> std::io::Result<()> + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let logger = logger.new(slog::o!("service" => name));
            if let Err(e) = f(logger.clone()) {
                error!(logger, "stopped: {e}");
            }
        })
        .expect("starting a thread")
}

/// Runs a PRUDP server with the protocols `register` adds.
fn run_server(logger: &Logger, ctx: &Context, register: impl FnOnce(&mut RVSecHandler<()>)) -> std::io::Result<()> {
    let mut handler = RVSecHandler::<()>::new(logger.clone());
    register(&mut handler);
    let mut registry = StreamHandlerRegistry::new(logger.clone());
    registry.register(
        VPort {
            stream_type: StreamType::RVSec,
            port: ctx.vport,
        },
        Box::new(handler),
    );
    // No handlers for expired or disconnected clients yet: nothing per player is kept.
    let mut server = Server::<fn(quazal::ClientInfo), fn(quazal::ClientInfo)>::new(logger.clone(), ctx, registry);
    server.user_handler = Some(address_echo);
    server.bind(ctx.listen)?;
    server.serve();
    Ok(())
}

/// The address echo: the game asks (a PRUDP user packet, on virtual port 1) where the server
/// sees it, to tell other players, and keeps asking until it's answered. The answer is its own
/// payload followed by `udp:/address=IP;port=PORT` and a NUL. `quazal` limits how often and how
/// much it answers, since the source of a UDP packet can be forged.
fn address_echo(logger: &Logger, packet: QPacket, client: SocketAddr, socket: &std::net::UdpSocket) {
    if packet.source.port != 1 || packet.destination.port != 1 {
        slog::debug!(logger, "user packet for ports {} -> {} ignored", packet.source.port, packet.destination.port);
        return;
    }
    let mut reply = packet.payload;
    reply.extend_from_slice(format!("udp:/address={};port={}\0", client.ip(), client.port()).as_bytes());
    if let Err(e) = socket.send_to(&reply, client) {
        slog::warn!(logger, "address echo to {client} failed: {e}");
    }
}
