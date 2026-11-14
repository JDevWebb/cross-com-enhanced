//! What the overlay draws: the panel and the toasts, in the launcher's colours
//! (`launcher-tauri/ui/src/theme.css`) and fonts (IBM Plex).
//!
//! Sizes are designed for 1080p and scale with the screen's height. A panic in here would end
//! the game (it runs inside the game's `Present`), so each frame's drawing is caught, and after
//! one the overlay stops drawing.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use hudhook::ImguiRenderLoop;
use hudhook::MessageFilter;
use imgui::Condition;
use imgui::FontId;
use imgui::StyleColor;
use imgui::Ui;
use log::error;
use log::info;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::WPARAM;
use windows::Win32::UI::WindowsAndMessaging::WM_ACTIVATEAPP;
use windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE;

use super::input;
use super::Reach;
use super::Status;

/// The screen height the sizes are designed for.
const DESIGN_HEIGHT: f32 = 1080.0;
const BODY_PX: f32 = 17.0;
const SMALL_PX: f32 = 14.0;
const HEADING_PX: f32 = 26.0;
/// How long a toast stays up.
const TOAST_FOR: Duration = Duration::from_secs(6);
/// How long a toast takes to fade in and out.
const FADE: f32 = 0.25;
/// The most toasts up at once.
const MAX_TOASTS: usize = 4;
/// `VK_ESCAPE`.
const VK_ESCAPE: u16 = 0x1B;

/// The launcher's colours (theme.css), as RGBA.
const fn rgb(hex: u32) -> [f32; 4] {
    [((hex >> 16) & 0xFF) as f32 / 255.0, ((hex >> 8) & 0xFF) as f32 / 255.0, (hex & 0xFF) as f32 / 255.0, 1.0]
}
const fn alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}
const BG: [f32; 4] = alpha(rgb(0x0b1216), 0.96);
const SURFACE: [f32; 4] = rgb(0x0f181d);
const CONTROL: [f32; 4] = rgb(0x15232a);
const CONTROL_HOVER: [f32; 4] = rgb(0x1b2d36);
const LINE: [f32; 4] = rgb(0x1a2830);
const CONTROL_LINE: [f32; 4] = rgb(0x263540);
const FG: [f32; 4] = rgb(0xe2edf1);
const SOFT: [f32; 4] = rgb(0xa6bbc4);
const MUTED: [f32; 4] = rgb(0x7f97a2);
const ACCENT: [f32; 4] = rgb(0x5fd0e6);
const ACCENT_DIM: [f32; 4] = alpha(rgb(0x5fd0e6), 0.35);
const SELECTED: [f32; 4] = rgb(0x0e2129);
const BAD: [f32; 4] = rgb(0xff7a6b);
const GOOD: [f32; 4] = rgb(0x7fe0a8);
const DIM: [f32; 4] = [0.0, 0.0, 0.0, 0.45];

/// The game lost focus since the last frame: an open panel closes.
static FOCUS_LOST: AtomicBool = AtomicBool::new(false);
/// The game's window has had a mouse message while the panel was open: Windows gives it the
/// mouse, and DirectInput's movement isn't needed for the overlay's cursor.
static WINDOW_MOUSE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Server,
    News,
    Account,
    Help,
}

const TABS: [(Tab, &str); 4] = [(Tab::Server, "Server"), (Tab::News, "News"), (Tab::Account, "Account"), (Tab::Help, "Help")];

struct Fonts {
    strong: FontId,
    small: FontId,
    heading: FontId,
}

// SAFETY: a `FontId` is a pointer into the font atlas, only used on the render thread, in the
// frames of the context that made it; hudhook only asks for the render loop to be Send and Sync.
unsafe impl Send for Fonts {}
unsafe impl Sync for Fonts {}

struct ShownToast {
    toast: super::Toast,
    /// When it was first drawn.
    since: Option<Instant>,
}

pub(super) struct Overlay {
    key: &'static str,
    vk: u16,
    open: bool,
    tab: Tab,
    key_was_down: bool,
    esc_was_down: bool,
    fonts: Option<Fonts>,
    /// The scale the fonts were made at, and the screen's now.
    font_scale: f32,
    s: f32,
    status: Status,
    status_version: u32,
    toasts: Vec<ShownToast>,
    /// The overlay stopped drawing after a panic.
    broken: bool,
    first_frame: bool,
}

impl Overlay {
    pub(super) fn new(key: &'static str, vk: u16) -> Self {
        let hint = super::Toast {
            text: format!("Cross-Com: {key} opens the overlay"),
            bad: false,
        };
        Self {
            key,
            vk,
            open: false,
            tab: Tab::Server,
            key_was_down: false,
            esc_was_down: false,
            fonts: None,
            font_scale: 1.0,
            s: 1.0,
            status: Status::default(),
            status_version: 0,
            toasts: vec![ShownToast { toast: hint, since: None }],
            broken: false,
            first_frame: true,
        }
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.s).round()
    }

    fn set_open(&mut self, open: bool, closing_key: u16) {
        if self.open == open {
            return;
        }
        self.open = open;
        if !open {
            input::hold_until_released(closing_key);
        }
        input::set_open(open);
        info!("Overlay {}", if open { "opened" } else { "closed" });
    }

    /// The overlay key toggles the panel and Esc closes it, while the game is in front.
    fn poll_keys(&mut self) {
        let key = input::key_down(self.vk);
        let esc = input::key_down(VK_ESCAPE);
        let in_front = input::game_in_front();
        if key && !self.key_was_down && in_front {
            self.set_open(!self.open, self.vk);
        } else if esc && !self.esc_was_down && self.open {
            self.set_open(false, VK_ESCAPE);
        }
        self.key_was_down = key;
        self.esc_was_down = esc;
    }

    /// The mouse's DirectInput movement as the overlay's cursor, when the game's window gets no
    /// mouse messages while the panel is open.
    fn direct_input_mouse(&self, io: &mut imgui::Io) {
        let (dx, dy, wheel, buttons) = input::take_mouse();
        if WINDOW_MOUSE.load(Ordering::Relaxed) {
            return;
        }
        let [w, h] = io.display_size;
        let [x, y] = io.mouse_pos;
        let (x, y) = if x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 {
            (x, y)
        } else {
            (w / 2.0, h / 2.0)
        };
        if dx != 0 || dy != 0 {
            io.add_mouse_pos_event([(x + dx as f32).clamp(0.0, w), (y + dy as f32).clamp(0.0, h)]);
        }
        if wheel != 0 {
            io.add_mouse_wheel_event([0.0, wheel as f32 / 120.0]);
        }
        for (bit, button) in [imgui::MouseButton::Left, imgui::MouseButton::Right, imgui::MouseButton::Middle].into_iter().enumerate() {
            let down = buttons & (1 << bit) != 0;
            if io.mouse_down[bit] != down {
                io.add_mouse_button_event(button, down);
            }
        }
    }

    fn frame(&mut self, ui: &Ui) {
        self.poll_keys();
        input::set_open(self.open);
        if let Some((version, status)) = super::status_since(self.status_version) {
            self.status_version = version;
            self.status = status;
        }
        self.toasts.extend(super::take_toasts().into_iter().map(|toast| ShownToast { toast, since: None }));
        if self.open {
            self.panel(ui);
        }
        self.show_toasts(ui);
    }

    // ---------- The panel ----------

    fn panel(&mut self, ui: &Ui) {
        let [w, h] = ui.io().display_size;
        // The game, dimmed behind the panel.
        ui.get_background_draw_list().add_rect([0.0, 0.0], [w, h], DIM).filled(true).build();
        let size = [self.px(720.0).min(w - 32.0), self.px(500.0).min(h - 32.0)];
        let _border = ui.push_style_color(StyleColor::Border, CONTROL_LINE);
        ui.window("##crosscom-panel")
            .no_decoration()
            .movable(false)
            .position([w / 2.0, h / 2.0], Condition::Always)
            .position_pivot([0.5, 0.5])
            .size(size, Condition::Always)
            .build(|| {
                self.header(ui);
                self.tabs(ui);
                ui.child_window("##crosscom-page").border(false).build(|| match self.tab {
                    Tab::Server => self.server_page(ui),
                    Tab::News => self.news_page(ui),
                    Tab::Account => self.account_page(ui),
                    Tab::Help => self.help_page(ui),
                });
            });
    }

    fn header(&self, ui: &Ui) {
        if let Some(fonts) = &self.fonts {
            let _f = ui.push_font(fonts.heading);
            ui.text_colored(ACCENT, "CROSS-COM");
        } else {
            ui.text_colored(ACCENT, "CROSS-COM");
        }
        let hint = format!("{} or Esc to close", self.key);
        let width = ui.calc_text_size(&hint)[0];
        ui.same_line_with_pos(ui.window_content_region_max()[0] - width);
        ui.text_colored(MUTED, hint);
        ui.spacing();
    }

    fn tabs(&mut self, ui: &Ui) {
        let width = self.px(110.0);
        for (i, (tab, name)) in TABS.into_iter().enumerate() {
            if i > 0 {
                ui.same_line();
            }
            let current = self.tab == tab;
            let _b = ui.push_style_color(StyleColor::Button, if current { SELECTED } else { CONTROL });
            let _t = ui.push_style_color(StyleColor::Text, if current { ACCENT } else { SOFT });
            if ui.button_with_size(name, [width, 0.0]) {
                self.tab = tab;
            }
            if current {
                let [x0, _] = ui.item_rect_min();
                let [x1, y1] = ui.item_rect_max();
                ui.get_window_draw_list().add_line([x0, y1 - 1.0], [x1, y1 - 1.0], ACCENT).thickness(self.px(2.0)).build();
            }
        }
        ui.spacing();
        ui.separator();
        ui.spacing();
    }

    fn label(&self, ui: &Ui, text: &str) {
        if let Some(fonts) = &self.fonts {
            let _f = ui.push_font(fonts.small);
            ui.text_colored(MUTED, text.to_uppercase());
        } else {
            ui.text_colored(MUTED, text.to_uppercase());
        }
    }

    /// A label and its value on one line.
    fn row(&self, ui: &Ui, label: &str, value: &str, color: [f32; 4]) {
        self.label(ui, label);
        ui.same_line_with_pos(self.px(170.0));
        ui.text_colored(color, value);
    }

    fn strong(&self, ui: &Ui, color: [f32; 4], text: &str) {
        if let Some(fonts) = &self.fonts {
            let _f = ui.push_font(fonts.strong);
            ui.text_colored(color, text);
        } else {
            ui.text_colored(color, text);
        }
    }

    fn server_page(&self, ui: &Ui) {
        let s = &self.status;
        let Some(server) = &s.server else {
            ui.text_wrapped("No server is set in crosscom.toml: the game looks for Ubisoft's servers, which are gone. Choose a server in the Cross-Com launcher.");
            return;
        };
        self.row(ui, "Address", server, FG);
        let (state, color) = match s.reach {
            Reach::Checking => ("Checking...", MUTED),
            Reach::Online => ("Online", GOOD),
            Reach::Offline => ("Offline", BAD),
        };
        self.row(ui, "Status", state, color);
        let info = s.info.as_ref();
        let version = info.and_then(|i| i.version.clone()).unwrap_or_else(|| "-".into());
        self.row(ui, "Server version", &version, FG);
        let players = info.and_then(|i| i.players_online).map_or_else(|| "-".into(), |n| n.to_string());
        self.row(ui, "Players online", &players, FG);
        if let Some(checked) = s.checked {
            let ago = checked.elapsed().as_secs();
            self.row(ui, "Checked", &if ago < 5 { "just now".into() } else { format!("{ago} s ago") }, MUTED);
        }
        ui.spacing();
        if let Some(error) = &s.error {
            let _t = ui.push_style_color(StyleColor::Text, BAD);
            ui.text_wrapped(format!("The server isn't answering: {error}. The overlay asks again every minute."));
        } else if let Some(why) = &s.no_info {
            let _t = ui.push_style_color(StyleColor::Text, MUTED);
            ui.text_wrapped(format!("The server answered, without its details ({why}); it may be an older version."));
        }
    }

    fn news_page(&self, ui: &Ui) {
        let news = self.status.info.as_ref().map(|i| i.news.as_slice()).unwrap_or_default();
        if news.is_empty() {
            let _t = ui.push_style_color(StyleColor::Text, MUTED);
            ui.text_wrapped(if self.status.info.is_some() {
                "No news."
            } else {
                "The server's news shows here once it has answered."
            });
            return;
        }
        for (i, item) in news.iter().enumerate() {
            if i > 0 {
                ui.spacing();
                ui.separator();
                ui.spacing();
            }
            self.strong(ui, FG, if item.title.is_empty() { "(untitled)" } else { &item.title });
            let mut meta = item.date.clone().unwrap_or_default();
            if let Some(shows) = item.shows.as_deref().filter(|s| !s.is_empty()) {
                if !meta.is_empty() {
                    meta.push_str("  -  ");
                }
                meta.push_str(&capitalised(shows));
            }
            if !meta.is_empty() {
                self.label(ui, &meta);
            }
            if !item.body.is_empty() {
                let _t = ui.push_style_color(StyleColor::Text, SOFT);
                ui.text_wrapped(&item.body);
            }
        }
    }

    fn account_page(&self, ui: &Ui) {
        let name = &crate::config().username;
        if name.is_empty() {
            ui.text_wrapped("No account is set in crosscom.toml. Sign in with the Cross-Com launcher.");
            return;
        }
        self.row(ui, "Account", name, FG);
        let (state, color) = if super::login_was_given() { ("Signing in", GOOD) } else { ("Not yet", MUTED) };
        self.row(ui, "Sign-in", state, color);
        ui.spacing();
        let _t = ui.push_style_color(StyleColor::Text, MUTED);
        ui.text_wrapped(if super::login_was_given() {
            "The game has this account's details. If the server refuses them, the game says so: check the name and password in the launcher."
        } else {
            "The game signs in when you go online."
        });
    }

    fn help_page(&self, ui: &Ui) {
        self.row(ui, "Overlay", &format!("{} opens and closes it, Esc closes it", self.key), FG);
        ui.spacing();
        ui.text_wrapped("While it's open the game doesn't get the mouse or keyboard. The key can be changed in the launcher (overlay_key in crosscom.toml, F1 to F12); overlay = false turns the overlay off.");
        ui.spacing();
        self.strong(ui, FG, "Reporting a problem");
        let _t = ui.push_style_color(StyleColor::Text, SOFT);
        ui.text_wrapped("Say what you did and what happened, and send crosscom.log from the game's folder (next to the game's exe) with it.");
    }

    // ---------- Toasts ----------

    fn show_toasts(&mut self, ui: &Ui) {
        let now = Instant::now();
        self.toasts.retain(|t| t.since.is_none_or(|since| now.duration_since(since) < TOAST_FOR));
        let [w, _] = ui.io().display_size;
        let margin = self.px(24.0);
        let gap = self.px(8.0);
        let mut y = margin;
        for (i, shown) in self.toasts.iter_mut().take(MAX_TOASTS).enumerate() {
            let since = *shown.since.get_or_insert(now);
            let age = now.duration_since(since).as_secs_f32();
            let left = TOAST_FOR.as_secs_f32() - age;
            let fade = (age / FADE).min(left / FADE).clamp(0.0, 1.0);
            let _a = ui.push_style_var(imgui::StyleVar::Alpha(fade));
            let _bg = ui.push_style_color(StyleColor::WindowBg, SURFACE);
            let _border = ui.push_style_color(StyleColor::Border, if shown.toast.bad { BAD } else { ACCENT_DIM });
            let height = ui
                .window(format!("##crosscom-toast-{i}"))
                .no_decoration()
                .no_inputs()
                .no_nav()
                .movable(false)
                .focus_on_appearing(false)
                .always_auto_resize(true)
                .position([w - margin, y], Condition::Always)
                .position_pivot([1.0, 0.0])
                .build(|| {
                    ui.text_colored(if shown.toast.bad { BAD } else { FG }, &shown.toast.text);
                    ui.window_size()[1]
                })
                .unwrap_or(0.0);
            y += height + gap;
        }
    }
}

fn capitalised(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn theme(style: &mut imgui::Style) {
    let c = &mut style.colors;
    c[StyleColor::Text as usize] = FG;
    c[StyleColor::TextDisabled as usize] = MUTED;
    c[StyleColor::WindowBg as usize] = BG;
    c[StyleColor::ChildBg as usize] = [0.0; 4];
    c[StyleColor::PopupBg as usize] = BG;
    c[StyleColor::Border as usize] = LINE;
    c[StyleColor::BorderShadow as usize] = [0.0; 4];
    c[StyleColor::FrameBg as usize] = CONTROL;
    c[StyleColor::FrameBgHovered as usize] = CONTROL_HOVER;
    c[StyleColor::FrameBgActive as usize] = CONTROL_HOVER;
    c[StyleColor::ScrollbarBg as usize] = [0.0; 4];
    c[StyleColor::ScrollbarGrab as usize] = CONTROL;
    c[StyleColor::ScrollbarGrabHovered as usize] = CONTROL_HOVER;
    c[StyleColor::ScrollbarGrabActive as usize] = CONTROL_HOVER;
    c[StyleColor::Button as usize] = CONTROL;
    c[StyleColor::ButtonHovered as usize] = CONTROL_HOVER;
    c[StyleColor::ButtonActive as usize] = CONTROL_HOVER;
    c[StyleColor::Header as usize] = ACCENT_DIM;
    c[StyleColor::HeaderHovered as usize] = ACCENT_DIM;
    c[StyleColor::HeaderActive as usize] = ACCENT_DIM;
    c[StyleColor::Separator as usize] = LINE;
    c[StyleColor::ResizeGrip as usize] = [0.0; 4];
    c[StyleColor::ResizeGripHovered as usize] = [0.0; 4];
    c[StyleColor::ResizeGripActive as usize] = [0.0; 4];
    c[StyleColor::TextSelectedBg as usize] = ACCENT_DIM;
    c[StyleColor::NavHighlight as usize] = ACCENT;
}

/// Sizes at 1080p, times `s`.
fn theme_sizes(style: &mut imgui::Style, s: f32) {
    style.window_rounding = 8.0 * s;
    style.child_rounding = 6.0 * s;
    style.frame_rounding = 4.0 * s;
    style.window_border_size = 1.0;
    style.child_border_size = 0.0;
    style.frame_border_size = 0.0;
    style.window_padding = [20.0 * s, 16.0 * s];
    style.frame_padding = [12.0 * s, 8.0 * s];
    style.item_spacing = [10.0 * s, 8.0 * s];
    style.item_inner_spacing = [8.0 * s, 6.0 * s];
    style.scrollbar_size = 10.0 * s;
    style.scrollbar_rounding = 5.0 * s;
}

/// The launcher's fonts, at `scale`; the first added is the body text.
fn add_fonts(ctx: &mut imgui::Context, scale: f32) -> Fonts {
    let mut add = |data: &'static [u8], px: f32| {
        ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data,
            size_pixels: (px * scale).round(),
            config: None,
        }])
    };
    add(include_bytes!("../../../../launcher-tauri/ui/public/fonts/IBMPlexSans-Regular.ttf"), BODY_PX);
    let strong = add(include_bytes!("../../../../launcher-tauri/ui/public/fonts/IBMPlexSans-SemiBold.ttf"), BODY_PX);
    let small = add(include_bytes!("../../../../launcher-tauri/ui/public/fonts/IBMPlexSans-SemiBold.ttf"), SMALL_PX);
    let heading = add(include_bytes!("../../../../launcher-tauri/ui/public/fonts/IBMPlexSansCondensed-Bold.ttf"), HEADING_PX);
    Fonts { strong, small, heading }
}

/// The scale for a screen `height` pixels high.
fn scale_for(height: f32) -> f32 {
    if height > 0.0 {
        (height / DESIGN_HEIGHT).clamp(0.6, 2.5)
    } else {
        1.0
    }
}

impl ImguiRenderLoop for Overlay {
    fn initialize(&mut self, ctx: &mut imgui::Context, _render_context: &mut dyn hudhook::RenderContext) {
        theme(ctx.style_mut());
        // hudhook has set the screen's size, and builds the font atlas after this.
        self.font_scale = scale_for(ctx.io().display_size[1]);
        self.s = self.font_scale;
        self.fonts = Some(add_fonts(ctx, self.font_scale));
        // Nothing to keep between games.
        ctx.set_ini_filename(None::<std::path::PathBuf>);
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    }

    fn before_render(&mut self, ctx: &mut imgui::Context, _render_context: &mut dyn hudhook::RenderContext) {
        if self.broken {
            return;
        }
        // Another program came in front: an open panel closes, freeing the mouse and keys.
        let focus_lost = FOCUS_LOST.swap(false, Ordering::SeqCst);
        if self.open && (focus_lost || !input::game_in_front()) {
            self.set_open(false, 0);
        }
        let height = ctx.io().display_size[1];
        if height > 0.0 {
            self.s = scale_for(height);
        }
        theme_sizes(ctx.style_mut(), self.s);
        let io = ctx.io_mut();
        io.font_global_scale = self.s / self.font_scale;
        // The game hides the system's cursor: the overlay draws its own while the panel is open.
        io.mouse_draw_cursor = self.open;
        if self.open {
            self.direct_input_mouse(io);
        } else {
            let _ = input::take_mouse();
        }
    }

    fn render(&mut self, ui: &mut Ui) {
        if self.broken {
            return;
        }
        if std::mem::take(&mut self.first_frame) {
            let [w, h] = ui.io().display_size;
            info!("Overlay drawing at {w}x{h}");
        }
        if std::panic::catch_unwind(AssertUnwindSafe(|| self.frame(ui))).is_err() {
            error!("Overlay: drawing failed; it's off until the game restarts");
            self.broken = true;
            self.open = false;
            input::set_open(false);
        }
    }

    fn message_filter(&self, _io: &imgui::Io) -> MessageFilter {
        if input::held_back() {
            MessageFilter::InputAll
        } else {
            MessageFilter::empty()
        }
    }

    fn on_wnd_proc(&self, hwnd: HWND, umsg: u32, wparam: WPARAM, _lparam: LPARAM) {
        input::seen_window(hwnd);
        if umsg == WM_ACTIVATEAPP && wparam.0 == 0 {
            FOCUS_LOST.store(true, Ordering::SeqCst);
        }
        if umsg == WM_MOUSEMOVE && self.open {
            WINDOW_MOUSE.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_come_from_hex() {
        assert_eq!(rgb(0xff0000), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(capitalised("multiplayer"), "Multiplayer");
        assert_eq!(capitalised(""), "");
    }
}
