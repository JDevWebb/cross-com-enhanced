//! The Cross-Com server for Ghost Recon: Future Soldier.
//!
//! Three services, each on its own thread:
//! - the online config, over HTTP, which tells the game where to sign in (and the launcher's
//!   API, and players' logs);
//! - the auth server, where the game signs in and gets a ticket for the secure server;
//! - the secure server, where the game's online services are.
//!
//! ```text
//! crosscom-server [--config crosscom-server.toml]
//! crosscom-server [--config crosscom-server.toml] add-account NAME PASSWORD
//! crosscom-server [--config crosscom-server.toml] logs [list | show REPORT]
//! crosscom-server [--config crosscom-server.toml] connections [DAYS]
//! ```

mod accounts;
mod api;
mod clans;
mod config;
mod connections;
mod dares;
mod fixed;
mod friends;
mod game_session;
mod invitations;
mod leaderboard;
mod logs;
mod maps;
mod menu;
mod nat;
mod news;
mod notify;
mod online;
mod online_config;
mod packs;
mod privileges;
mod proxy;
mod rate_limit;
mod reach;
mod secure;
mod sessions;
mod signup;
mod social;
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
    AddAccount {
        name: String,
        password: String,
    },
    /// The players' logs kept: all of them, newest first.
    ListLogs,
    /// One report's log.
    ShowLog(String),
    /// The connection records of the last so many days, summed up (`connections`).
    Connections(u64),
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
        [cmd] if cmd == "logs" => Command::ListLogs,
        [cmd, sub] if cmd == "logs" && sub == "list" => Command::ListLogs,
        [cmd, sub, id] if cmd == "logs" && sub == "show" => Command::ShowLog(id.clone()),
        [cmd] if cmd == "connections" => Command::Connections(30),
        [cmd, days] if cmd == "connections" => Command::Connections(days.parse().map_err(|_| format!("{days} isn't a number of days"))?),
        _ => return Err("usage: crosscom-server [--config FILE] [add-account NAME PASSWORD | logs [list | show REPORT] | connections [DAYS]]".into()),
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
    let config = match Config::load(&config_path).and_then(|c| c.with_env(|name| std::env::var(name).ok())) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    match &command {
        Command::ListLogs => {
            print!("{}", logs::listing(&logs::list(&config.logs_dir)));
            return ExitCode::SUCCESS;
        }
        Command::ShowLog(id) => {
            let Some(path) = logs::find(&config.logs_dir, id) else {
                eprintln!("no report {id} in {}", config.logs_dir.display());
                return ExitCode::FAILURE;
            };
            return match std::fs::read(&path) {
                Ok(log) => {
                    use std::io::Write;
                    let _ = std::io::stdout().write_all(&log);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("couldn't read {}: {e}", path.display());
                    ExitCode::FAILURE
                }
            };
        }
        Command::Serve | Command::AddAccount { .. } | Command::Connections(_) => {}
    }
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
        Command::Connections(days) => {
            let records = connections::Connections::open(&config.database).and_then(|c| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                c.since(now.saturating_sub(days * 24 * 3600))
            });
            match records {
                Ok(records) => {
                    let name = |pid: u32| accounts.name(pid).ok().flatten().unwrap_or_else(|| format!("{pid:#x}"));
                    print!("{}", connections::summary(&records, days, name));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("couldn't read the connection records in {}: {e}", config.database.display());
                    ExitCode::FAILURE
                }
            }
        }
        Command::ListLogs | Command::ShowLog(_) => ExitCode::SUCCESS,
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
    info!(logger, "Cross-Com server {}; players connect to {public_ip}", env!("CROSSCOM_RELEASE"));

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
    // Failed sign-ins count together, from the game and from the launcher.
    let failed_sign_ins = Arc::new(rate_limit::FailedSignIns::default());
    let online = Arc::new(online::Online::default());
    let stats = match stats::Store::open(&config.database) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!(logger, "couldn't open the statistics in {}: {e}", config.database.display());
            return ExitCode::FAILURE;
        }
    };
    // The game sessions (secure server); the auth server takes out a restarted host's lobbies.
    let sessions = Arc::new(sessions::Sessions::default());
    match connections::Connections::open(&config.database) {
        Ok(records) => connections::start(&logger, records, Arc::clone(&sessions)),
        Err(e) => error!(logger, "couldn't open the connection records in {}: {e}; joins aren't recorded", config.database.display()),
    }
    let http = online_config::Http {
        content: online_config::content(public_ip, config.auth_port),
        reach: reach::Reach::new(Arc::clone(&sessions)),
        signup: config.signup.then(|| signup::SignUp::new(Arc::clone(&accounts), config.signup_code.clone())),
        api: api::Api::new(Arc::clone(&accounts), Arc::clone(&failed_sign_ins), config.news.clone(), Arc::clone(&online))
            .with_stats(Arc::clone(&stats))
            .with_udp_ports(vec![config.auth_port, config.secure_port]),
        // Checked as the settings were read.
        proxies: config.trusted_proxies().unwrap_or_default(),
        logs: logs::Logs::new(config.logs(), Arc::clone(&accounts), Arc::clone(&online)),
    };
    if config.accept_logs {
        info!(logger, "Players' logs kept in {}", config.logs_dir.display());
    }
    if !config.trusted_proxies.is_empty() {
        info!(logger, "HTTP behind a proxy at {}", config.trusted_proxies.join(", "));
    }
    if config.signup && config.signup_code.is_none() {
        info!(logger, "Sign-up is open to anyone who can reach this server (no signup_code)");
    }
    let friends = match friends::Friends::open(&config.database) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!(logger, "couldn't open the friends in {}: {e}", config.database.display());
            return ExitCode::FAILURE;
        }
    };
    let clans = match clans::Store::open(&config.database) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!(logger, "couldn't open the squads in {}: {e}", config.database.display());
            return ExitCode::FAILURE;
        }
    };
    let dares = match dares::Store::open(&config.database) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!(logger, "couldn't open the challenges in {}: {e}", config.database.display());
            return ExitCode::FAILURE;
        }
    };
    let news = config.news.clone();
    // The DLC packs players say they have (at sign-in, on the auth server) and their lobbies use
    // (on the secure server): searches go by them.
    let packs = Arc::new(packs::Packs::default());

    let threads = [
        spawn("online config", logger.clone(), move |logger| online_config::serve(&logger, http_listen, http)),
        spawn("auth", logger.clone(), {
            let accounts = Arc::clone(&accounts);
            let packs = Arc::clone(&packs);
            let sessions = Arc::clone(&sessions);
            move |logger| {
                run_server(&logger, &auth, None, |handler| {
                    handler.register_protocol(ticket::new_protocol(accounts, failed_sign_ins, packs, sessions));
                })
            }
        }),
        spawn("secure", logger.clone(), move |logger| {
            run_server(&logger, &secure, Some(online), |handler| {
                handler.register_protocol(secure::new_protocol(Arc::clone(&sessions)));
                handler.register_protocol(nat::new_protocol(Arc::clone(&sessions)));
                handler.register_protocol(game_session::new_protocol(
                    Arc::clone(&sessions),
                    Arc::clone(&accounts),
                    Arc::clone(&friends),
                    Arc::clone(&packs),
                ));
                handler.register_protocol(Box::new(maps::MapList::new(Arc::clone(&packs))));
                handler.register_protocol(privileges::new_protocol());
                let social = social::protocols(Arc::clone(&friends), Arc::clone(&accounts));
                for protocol in menu::protocols().into_iter().chain(fixed::protocols()).chain(social) {
                    handler.register_protocol(protocol);
                }
                handler.register_protocol(Box::new(stats::PlayerStatistics::new(Arc::clone(&stats))));
                handler.register_protocol(Box::new(dares::GameEvents::new(Arc::clone(&dares), Arc::clone(&friends))));
                handler.register_protocol(Box::new(dares::OverlordDare::new(Arc::clone(&dares))));
                handler.register_protocol(Box::new(news::OverlordNews::new(news.clone())));
                handler.register_protocol(Box::new(clans::Clans::new(Arc::clone(&clans), Arc::clone(&accounts))));
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

/// Runs a PRUDP server with the protocols `register` adds, keeping `online` up to date with who
/// is signed in to it.
fn run_server(logger: &Logger, ctx: &Context, online: Option<Arc<online::Online>>, register: impl FnOnce(&mut RVSecHandler<()>)) -> std::io::Result<()> {
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
    // A player's last connection closing (or timing out) signs them out. When they sign in
    // from a new address, their old connections close first and the new sign-in counts after.
    // On the secure server, where the sessions are, its joins end too (`connections`).
    let gone = {
        let online = online.clone();
        let logger = logger.clone();
        move |ci: quazal::ClientInfo| {
            if let (Some(online), Some(pid)) = (&online, ci.user_id) {
                online.gone(pid);
                connections::gone(&logger, pid);
            }
        }
    };
    let mut server = Server::new(logger.clone(), ctx, registry);
    server.expired_client_handler = Some(gone.clone());
    server.disconnect_handler = Some(gone);
    if let Some(online) = online {
        server.login_handler = Some(Box::new(move |pid, client: SocketAddr| online.signed_in(pid, client.ip())));
    }
    server.user_handler = Some(address_echo);
    // Every game also signs in to the shared telemetry account (on a connection of its own): one
    // game's sign-in mustn't close everyone else's. Starting two together left both signed out.
    server.newest_sign_in_wins = |pid| pid != ticket::TRACKING_PID;
    // Its password is built into the game, so signing in as it proves nothing: its connections
    // stay within the limits on connections that haven't signed in (32 an address), or anyone
    // could hold as many as an address may have in all (256), and for as long as a player's.
    // A game holds one beside its own, so a household stays well within them.
    server.shared_account = |pid| pid == ticket::TRACKING_PID;
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
