// The way to the Rust side (src-tauri/src/commands.rs) and to the window. In a plain browser
// (`npm run dev`), sample answers instead, so the screens can be looked at alone; `?fresh` shows
// a first start.
const tauri = window.__TAURI__;
export const inTauri = Boolean(tauri);

const fresh = location.search.includes('fresh');
const sample = {
  info: {
    product: 'Cross-Com Enhanced', release: '0.1.0',
    game_dir: fresh ? null : "C:\\Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher\\games\\Tom Clancy's Ghost Recon Future Soldier",
    client: fresh ? 'none' : 'yes', carries_client: true,
    server: fresh ? null : 'play.example.org', account: fresh ? null : 'Kozak',
    skip_intro: true, dx9: false, ready: !fresh,
  },
  find_game: "C:\\Program Files (x86)\\Ubisoft\\Ubisoft Game Launcher\\games\\Tom Clancy's Ghost Recon Future Soldier",
  check_game: (a) => a.dir,
  check_server: () => '203.0.113.10:21030',
  setup: () => 'The game is set up.',
  play: () => 'The game is starting.',
  restore: () => "The game's own files are back.",
  set_options: () => null,
};

/** Calls a command; in the browser, answers from the samples after a short wait. */
export async function call(command, args = {}) {
  if (inTauri) return tauri.core.invoke(command, args);
  await new Promise((r) => setTimeout(r, 400));
  const answer = sample[command];
  return typeof answer === 'function' ? answer(args) : answer;
}

/** Asks the player for a folder; null if they don't choose one. */
export async function chooseFolder(title) {
  if (inTauri) return tauri.dialog.open({ directory: true, title });
  return sample.find_game;
}

/** The window, for the title bar's buttons. */
export const win = {
  minimize: () => tauri?.window.getCurrentWindow().minimize(),
  toggleMaximize: () => tauri?.window.getCurrentWindow().toggleMaximize(),
  close: () => tauri?.window.getCurrentWindow().close(),
};
