//! The launcher's window: the game's folder, the server, the account, and buttons to set the
//! game up and play.

use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;

use crosscom_client_common::config::Config;
use crosscom_launcher::game;
use crosscom_launcher::game::Installed;
use crosscom_launcher::server;
use crosscom_launcher::settings;
use crosscom_launcher::settings::Launcher;
use eframe::egui;

/// What the game is given as its CD key when none was set: the server doesn't check it.
const NO_CD_KEY: &str = "CROSSCOM";

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([520.0, 560.0]).with_min_inner_size([420.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("Cross-Com", options, Box::new(|_cc| Ok(Box::new(App::new()))))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Account {
    Existing,
    New,
}

/// A message for the player, and whether it's bad news.
struct Status {
    text: String,
    error: bool,
}

/// Work done off the window's thread (server calls), and what it found.
enum Done {
    Checked(Result<String, String>),
    SetUp(Result<String, String>),
}

struct App {
    prefs_path: PathBuf,
    prefs: Launcher,
    /// Where the launcher's copy of the client DLLs is: next to it.
    client_dir: PathBuf,
    server: String,
    account: Account,
    name: String,
    password: String,
    code: String,
    skip_intro: bool,
    cd_key: String,
    status: Option<Status>,
    busy: Option<mpsc::Receiver<Done>>,
}

fn prefs_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    base.join("Cross-Com").join("launcher.toml")
}

/// Where to look for the game: the Ubisoft launcher's and Steam's records, then the usual folders
/// on every drive.
fn candidates() -> Vec<PathBuf> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let mut found = Vec::new();
    if let Ok(installs) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs") {
        for id in installs.enum_keys().flatten() {
            if let Ok(dir) = installs.open_subkey(&id).and_then(|k| k.get_value::<String, _>("InstallDir")) {
                found.push(PathBuf::from(dir));
            }
        }
    }
    let mut roots: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    if let Ok(steam) = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Valve\Steam")
        .and_then(|k| k.get_value::<String, _>("SteamPath"))
    {
        if let Some(parent) = Path::new(&steam).parent() {
            roots.push(parent.to_path_buf());
        }
    }
    roots.extend((b'C'..=b'Z').map(|d| PathBuf::from(format!("{}:\\", d as char))).filter(|p| p.exists()));
    found.extend(game::usual_places(&roots));
    found
}

impl App {
    fn new() -> Self {
        let prefs_path = prefs_path();
        let mut prefs = Launcher::load(&prefs_path);
        if prefs.game_dir.as_deref().is_none_or(|d| !game::is_game_dir(d)) {
            prefs.game_dir = game::find(&candidates());
        }
        let client_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
        let config = prefs.game_dir.as_deref().map(settings::client).unwrap_or_default();
        Self {
            prefs_path,
            prefs,
            client_dir,
            server: config.server.clone().unwrap_or_default(),
            account: if config.username.is_empty() { Account::New } else { Account::Existing },
            name: config.username,
            password: config.password,
            code: String::new(),
            skip_intro: config.skip_intro_videos,
            cd_key: config.cd_key,
            status: None,
            busy: None,
        }
    }

    fn say(&mut self, result: Result<String, String>) {
        self.status = Some(match result {
            Ok(text) => Status { text, error: false },
            Err(text) => Status { text, error: true },
        });
    }

    /// Runs `work` off the window's thread; its result comes back through `busy`.
    fn spawn(&mut self, ctx: &egui::Context, work: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
        self.busy = Some(rx);
    }

    fn config(&self) -> Config {
        Config {
            server: Some(server::host(&self.server)),
            username: self.name.trim().to_owned(),
            password: self.password.clone(),
            cd_key: if self.cd_key.is_empty() { NO_CD_KEY.into() } else { self.cd_key.clone() },
            skip_intro_videos: self.skip_intro,
            ..Config::default()
        }
    }

    /// Makes the account if asked, then installs the client and writes its settings.
    fn set_up(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.prefs.game_dir.clone() else {
            return self.say(Err("Choose the game's folder first.".into()));
        };
        if self.server.trim().is_empty() || self.name.trim().is_empty() || self.password.is_empty() {
            return self.say(Err("Fill in the server, your name and your password.".into()));
        }
        let config = self.config();
        let (client, new, code, prefs, prefs_path) = (
            self.client_dir.clone(),
            self.account == Account::New,
            self.code.clone(),
            self.prefs.clone(),
            self.prefs_path.clone(),
        );
        self.spawn(ctx, move || {
            Done::SetUp((|| {
                let host = config.server.clone().unwrap_or_default();
                if new {
                    server::sign_up(&host, &config.username, &config.password, &code)?;
                }
                game::install(&dir, &client)?;
                settings::save_client(&dir, &config)?;
                prefs.save(&prefs_path)?;
                Ok(if new {
                    format!("Account {} made and the game set up. Press Play.", config.username)
                } else {
                    "The game is set up. Press Play.".into()
                })
            })())
        });
    }

    fn play(&mut self) {
        let Some(dir) = &self.prefs.game_dir else { return };
        let exe = dir.join(if self.prefs.dx9 { game::EXE_DX9 } else { game::EXE_DX11 });
        let result = std::process::Command::new(&exe).current_dir(dir).spawn();
        self.say(result.map(|_| "The game is starting.".into()).map_err(|e| format!("Couldn't start {}: {e}", exe.display())));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.busy {
            if let Ok(done) = rx.try_recv() {
                self.busy = None;
                match done {
                    Done::Checked(r) => self.say(r.map(|at| format!("The server answers; the game will sign in at {at}."))),
                    Done::SetUp(r) => self.say(r),
                }
            }
        }
        let busy = self.busy.is_some();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Cross-Com");
            ui.label("Ghost Recon: Future Soldier multiplayer on community servers.");
            ui.add_space(8.0);

            ui.group(|ui| {
                ui.strong("Game");
                match &self.prefs.game_dir {
                    Some(dir) => {
                        ui.label(dir.display().to_string());
                        let state = match game::installed(dir, &self.client_dir) {
                            Installed::No => "Cross-Com isn't installed in it yet.",
                            Installed::Yes => "Cross-Com is installed.",
                            Installed::Other => "Another version of Cross-Com is installed; setting up updates it.",
                        };
                        ui.weak(state);
                    }
                    None => {
                        ui.label("Not found. Choose the folder that holds \"Future Soldier DX11.exe\".");
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Choose folder...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            if game::is_game_dir(&dir) {
                                self.prefs.game_dir = Some(dir);
                                self.status = None;
                            } else {
                                self.say(Err("That folder doesn't hold the game.".into()));
                            }
                        }
                    }
                    if ui.button("Look again").clicked() {
                        self.prefs.game_dir = game::find(&candidates());
                    }
                });
                ui.checkbox(&mut self.prefs.dx9, "Use DirectX 9 (for older graphics cards)");
                ui.checkbox(&mut self.skip_intro, "Skip the intro videos");
            });

            ui.group(|ui| {
                ui.strong("Server");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.server).hint_text("address or name").desired_width(260.0));
                    if ui.add_enabled(!busy && !self.server.trim().is_empty(), egui::Button::new("Check")).clicked() {
                        let host = self.server.clone();
                        self.spawn(ctx, move || Done::Checked(server::check(&host)));
                    }
                });
            });

            ui.group(|ui| {
                ui.strong("Account");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.account, Account::Existing, "I have an account");
                    ui.radio_value(&mut self.account, Account::New, "Make one");
                });
                egui::Grid::new("account").num_columns(2).show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(220.0));
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(220.0));
                    ui.end_row();
                    if self.account == Account::New {
                        ui.label("Sign-up code");
                        ui.add(egui::TextEdit::singleline(&mut self.code).hint_text("if the server has one").desired_width(220.0));
                        ui.end_row();
                    }
                });
                if self.account == Account::New {
                    ui.weak("Pick a password you use nowhere else: it's sent to the server unencrypted, as the game's own sign-in is.");
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !busy && self.prefs.game_dir.is_some();
                if ui.add_enabled(ready, egui::Button::new("Set up")).clicked() {
                    self.set_up(ctx);
                }
                let installed = self.prefs.game_dir.as_deref().is_some_and(|d| game::installed(d, &self.client_dir) != Installed::No);
                if ui.add_enabled(ready && installed, egui::Button::new("Play")).clicked() {
                    self.play();
                }
                if ui.add_enabled(ready && installed, egui::Button::new("Restore the game's files")).clicked() {
                    if let Some(dir) = self.prefs.game_dir.clone() {
                        self.say(game::restore(&dir).map(|()| "The game's own files are back.".into()));
                    }
                }
                if busy {
                    ui.spinner();
                }
            });

            if let Some(status) = &self.status {
                ui.add_space(8.0);
                if status.error {
                    ui.colored_label(ui.visuals().error_fg_color, &status.text);
                } else {
                    ui.label(&status.text);
                }
            }
        });
    }
}
