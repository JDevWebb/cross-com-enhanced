//! The README's screenshots (`crosscom-launcher --shots DIR`, run by scripts/launcher-shots.sh):
//! Play, Setup and Settings filled in by the launcher's own code (ui.rs) from made-up state, and
//! drawn by Slint's software renderer into memory, so no screen, game or server is needed and
//! nothing reaches the network. Each is a PNG named as the README uses it.

use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;

use crosscom_client_net::check;
use crosscom_launcher::dlc;
use crosscom_launcher::server;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::software_renderer::RepaintBufferType;
use slint::ComponentHandle;
use slint::ModelRc;
use slint::Rgb8Pixel;
use slint::SharedString;
use slint::VecModel;

use crate::core;
use crate::core::ClientState;
use crate::ui;
use crate::AppWindow;

/// The window's size in the pictures: the launcher's own, and taller for Setup, so all of its
/// step shows.
const WIDTH: u32 = 1180;
const HEIGHT: u32 = 780;
const TALL: u32 = 940;
/// Settings › About, whole, down to its small print and Slint's attribution.
const ABOUT_HEIGHT: u32 = 1560;
/// Where the made-up game is.
const GAME: &str = r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\games\Tom Clancy's Ghost Recon Future Soldier";
const NAME: &str = "Kozak";
/// The player's identity, as Settings shows it.
const IDENTITY: &str = "K7QX-M2RA";

fn s(text: impl AsRef<str>) -> SharedString {
    SharedString::from(text.as_ref())
}

/// A picture: its name, the screen as it shows, and the window's height.
type Shot = (&'static str, fn(&AppWindow), u32);

/// The platform for the pictures: one window, drawn in software.
struct Shots {
    window: Rc<MinimalSoftwareWindow>,
}

impl slint::platform::Platform for Shots {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

/// Everything set up: the community test network, signed in, the client installed.
fn ready() -> core::Info {
    core::Info {
        game_dir: Some(PathBuf::from(GAME)),
        client: ClientState::Yes,
        carries_client: true,
        server: Some(server::COMMUNITY.into()),
        account: Some(NAME.into()),
        skip_intro: true,
        dx9: false,
        overlay: true,
        overlay_key: "F5".into(),
        min_players: core::DEFAULT_MIN_PLAYERS,
        send_logs: true,
        port_forwarding: true,
        dlc: packs(&[]),
    }
}

/// Every pack, installed except the ones `missing` names (by folder).
fn packs(missing: &[&str]) -> Vec<(dlc::Pack, dlc::State)> {
    dlc::PACKS
        .iter()
        .map(|&p| (p, if missing.contains(&p.folder) { dlc::State::Missing } else { dlc::State::Installed }))
        .collect()
}

fn news(title: &str, body: &str, date: &str, shows: Option<&str>) -> server::News {
    server::News {
        title: title.into(),
        body: body.into(),
        date: date.into(),
        shows: shows.map(str::to_owned),
    }
}

fn play(w: &AppWindow) {
    play_with(w, ready());
}

/// The Play screen of a game without Khyber Strike: the DLC warning under Play.
fn play_missing_dlc(w: &AppWindow) {
    play_with(w, core::Info { dlc: packs(&["dlc3"]), ..ready() });
}

fn play_with(w: &AppWindow, info: core::Info) {
    w.set_info(ui::info_view(&info));
    w.set_screen(s("play"));
    w.set_play_tab(s("status"));
    w.set_id_short(s(IDENTITY));
    let info = server::Info {
        version: "0.1.0".into(),
        players_online: 7,
        news: vec![
            news("Double XP weekend", "All weekend, every ranked match.", "2026-11-20", Some("multiplayer")),
            news(
                "Welcome to the community test network",
                "Thanks for testing Cross-Com Enhanced. Logs from your games help fix what breaks; accounts and stats may be reset between test builds.",
                "2026-11-15",
                None,
            ),
        ],
    };
    w.set_server_state(s("up"));
    w.set_server_detail(s(ui::server_detail(Some(&info))));
    w.set_news(ModelRc::new(VecModel::from(info.news.iter().map(ui::news_row).collect::<Vec<_>>())));
    let check = |label, value: &str, tone, detail: &str| core::Check {
        label,
        value: value.into(),
        tone,
        detail: detail.into(),
        action: "",
    };
    ui::show_checks(
        w,
        &[
            check("Firewall", "Game allowed", "good", "Windows lets other players reach your game."),
            check("Port forwarding", "On", "good", "Asked of your router (UPnP, PCP or NAT-PMP) while the game runs."),
            check("Last game", "Open · 2 h ago", "good", "Any player can join the matches you host."),
        ],
    );
}

/// A first start: the game found, the community test network answering, and the account step: no
/// account for this identity there yet, so a name for a new one.
fn setup(w: &AppWindow) {
    let fresh = core::Info {
        server: None,
        account: None,
        client: ClientState::No,
        ..ready()
    };
    w.set_info(ui::info_view(&fresh));
    w.set_screen(s("setup"));
    w.set_setup_game_dir(s(GAME));
    w.set_setup_community(true);
    w.set_setup_server_ok(s("203.0.113.10:21030"));
    w.set_setup_account(s(""));
    w.set_setup_name(s(NAME));
    w.set_setup_skip_intro(true);
    w.set_setup_account_done(true);
    w.set_setup_missing(s(""));
    w.set_setup_dlc(ui::dlc_rows(&packs(&[])));
    w.set_setup_dlc_note(s(""));
    w.set_setup_step(2);
}

fn settings(w: &AppWindow) {
    let info = ready();
    let view = ui::info_view(&info);
    w.set_info(view.clone());
    w.set_screen(s("settings"));
    w.set_settings_section(s("network"));
    w.set_opt_min_players(view.min_players - 2);
    w.set_opt_skip_intro(view.skip_intro);
    w.set_opt_dx9(view.dx9);
    w.set_opt_overlay(view.overlay);
    w.set_opt_overlay_key(view.overlay_key);
    w.set_opt_send_logs(view.send_logs);
    w.set_opt_port_forwarding(view.port_forwarding);
    let report = check::Report {
        seen: Some("203.0.113.24:51022".parse().expect("an address")),
        nat: check::Nat::Consistent,
        router: Ok(check::Router {
            how: "UPnP",
            public: "203.0.113.24:51023".parse().expect("an address"),
        }),
        forward_works: Some(true),
        unchecked: None,
        problem: None,
    };
    ui::show_connection(w, &Ok(report));
    let status = core::LogsStatus {
        last: Some(("20261115-123456-ab12cd".into(), 1_794_746_096)),
        said: None,
        sending: false,
    };
    w.set_logs_last(s(ui::logs_last(&status)));
    w.set_logs_said(s("Logs sent: report 20261115-123456-ab12cd."));
    // A made-up identity, just copied.
    w.set_id_short(s(IDENTITY));
    w.set_id_error(s(""));
    w.set_id_said(s(ui::IDENTITY_COPIED));
    w.set_id_failed(false);
}

fn about(w: &AppWindow) {
    w.set_info(ui::info_view(&ready()));
    w.set_screen(s("settings"));
    w.set_settings_section(s("about"));
}

/// Settings › Identity, just copied.
fn identity(w: &AppWindow) {
    settings(w);
    w.set_settings_section(s("identity"));
}

pub fn run(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Shots { window: window.clone() })).map_err(|e| e.to_string())?;
    let w = AppWindow::new().map_err(|e| e.to_string())?;
    w.global::<crate::Theme>().set_reduced_motion(true);
    w.set_release(s(core::RELEASE));
    w.set_community_host(s(server::COMMUNITY));
    w.set_community_name(s(server::COMMUNITY_NAME));
    w.set_community_status(s(server::COMMUNITY_STATUS));
    w.set_loaded(true);
    window.set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    w.show().map_err(|e| e.to_string())?;

    let shots: [Shot; 6] = [
        ("launcher-play", play, HEIGHT),
        ("launcher-play-missing-dlc", play_missing_dlc, HEIGHT),
        ("launcher-setup", setup, TALL),
        ("launcher-settings", settings, HEIGHT),
        ("launcher-settings-identity", identity, HEIGHT),
        ("launcher-about", about, ABOUT_HEIGHT),
    ];
    for (name, set, height) in shots {
        window.set_size(slint::PhysicalSize::new(WIDTH, height));
        set(&w);
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        let mut pixels = vec![Rgb8Pixel::default(); (WIDTH * height) as usize];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, WIDTH as usize);
        });
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let image = image::RgbImage::from_raw(WIDTH, height, bytes).ok_or("the picture's size")?;
        let path = dir.join(format!("{name}.png"));
        image.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("{} {WIDTH}x{height}", path.display());
    }
    Ok(())
}
