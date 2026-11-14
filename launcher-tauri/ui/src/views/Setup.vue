<template>
  <section class="page">
    <div class="inner">
      <p class="kicker">{{ first ? 'First deployment' : 'Set up' }}</p>
      <h1 class="title">{{ first ? 'Get Cross-Com ready' : 'Game, server and account' }}</h1>
      <p class="soft lead">Four steps, then the launcher does the rest: it installs Cross-Com's client into the game (keeping the game's own files), makes your account and writes your settings.</p>

      <ol class="steps">
        <li class="card" :class="{ done: game.dir }">
          <header><span class="n">01</span><h2>The game</h2><span v-if="game.dir" class="ok">Found</span></header>
          <p v-if="game.finding" class="muted"><i class="spin"></i> Looking for Ghost Recon: Future Soldier…</p>
          <p v-else-if="game.dir" class="path">{{ game.dir }}</p>
          <p v-else class="soft">Not found where Ubisoft Connect or Steam install it. Choose the folder that holds "Future Soldier DX11.exe".</p>
          <p v-if="game.error" class="bad">{{ game.error }}</p>
          <button type="button" class="btn" @click="chooseGame">{{ game.dir ? 'Choose another folder' : 'Choose folder' }}</button>
        </li>

        <li class="card" :class="{ done: server.ok }">
          <header><span class="n">02</span><h2>The server</h2><span v-if="server.ok" class="ok">Answering</span></header>
          <div class="row">
            <label class="field grow">
              <span class="caps">Address</span>
              <input v-model.trim="server.host" placeholder="play.example.org or 203.0.113.10" spellcheck="false" @input="server.ok = null" @keyup.enter="checkServer" />
            </label>
            <button type="button" class="btn" :disabled="!server.host || server.checking" @click="checkServer">
              <i v-if="server.checking" class="spin"></i><span v-else>Check</span>
            </button>
          </div>
          <p v-if="server.ok" class="muted">The game will sign in at {{ server.ok }}.</p>
          <p v-if="server.error" class="bad">{{ server.error }}</p>
        </li>

        <li class="card" :class="{ done: accountDone }">
          <header><span class="n">03</span><h2>Your account</h2></header>
          <div class="tabs" role="tablist">
            <button type="button" role="tab" :aria-selected="account.new" :class="{ on: account.new }" @click="account.new = true">Make one</button>
            <button type="button" role="tab" :aria-selected="!account.new" :class="{ on: !account.new }" @click="account.new = false">Sign in</button>
          </div>
          <div class="grid">
            <label class="field">
              <span class="caps">Name</span>
              <input v-model.trim="account.name" maxlength="32" spellcheck="false" autocomplete="username" />
            </label>
            <label class="field">
              <span class="caps">Password</span>
              <input v-model="account.password" type="password" maxlength="79" :autocomplete="account.new ? 'new-password' : 'current-password'" />
            </label>
            <label v-if="account.new" class="field">
              <span class="caps">Password again</span>
              <input v-model="account.again" type="password" maxlength="79" autocomplete="new-password" />
            </label>
            <label v-if="account.new" class="field">
              <span class="caps">Sign-up code <span class="muted">(if the server has one)</span></span>
              <input v-model.trim="account.code" spellcheck="false" />
            </label>
          </div>
          <p v-if="account.new" class="muted small">Letters, digits, "-", "_" and "." for your name, which other players see. Use a password you use nowhere else.</p>
          <p v-else class="muted small">The server checks your name and password before the game uses them.</p>
        </li>

        <li class="card done">
          <header><span class="n">04</span><h2>Game options</h2></header>
          <label class="check"><input v-model="options.skipIntro" type="checkbox" /> Skip the intro videos</label>
          <label class="check"><input v-model="options.dx9" type="checkbox" /> Use DirectX 9 (for older graphics cards)</label>
        </li>
      </ol>

      <div class="go">
        <button type="button" class="btn primary big" :disabled="!ready || busy" @click="run">
          <i v-if="busy" class="spin"></i>
          {{ busy ? 'Setting up…' : first ? 'Set up and get ready' : 'Save and set up' }}
        </button>
        <button v-if="!first" type="button" class="btn" :disabled="busy" @click="$emit('cancel')">Cancel</button>
        <p v-if="!ready && !busy" class="muted">{{ missing }}</p>
        <p v-if="error" class="bad">{{ error }}</p>
      </div>
    </div>
  </section>
</template>

<script setup>
// The guided setup: the game's folder (found on its own when it can be), the server (checked),
// the account (made here, or an existing one, checked with the server) and the game options; then one press sets it all up.
import { computed, onMounted, reactive, ref } from 'vue';
import { call, chooseFolder } from '../lib/tauri.js';

const props = defineProps({ info: { type: Object, required: true }, first: Boolean });
const emit = defineEmits(['finished', 'cancel']);

const game = reactive({ dir: props.info.game_dir, finding: false, error: null });
const server = reactive({ host: props.info.server || '', ok: null, checking: false, error: null });
const account = reactive({ new: !props.info.account, name: props.info.account || '', password: '', again: '', code: '' });
const options = reactive({ skipIntro: props.first ? true : props.info.skip_intro, dx9: props.info.dx9 });
const busy = ref(false);
const error = ref(null);

const NAME = /^[A-Za-z0-9._-]{1,32}$/;
const accountDone = computed(() => NAME.test(account.name) && account.password && (!account.new || account.password === account.again));
const ready = computed(() => game.dir && server.host && accountDone.value);
const missing = computed(() => {
  if (!game.dir) return 'Choose the game’s folder.';
  if (!server.host) return 'Enter the server’s address.';
  if (!NAME.test(account.name)) return 'Enter a name: letters, digits, "-", "_" or ".".';
  if (!account.password) return 'Enter your password.';
  if (account.new && account.password !== account.again) return 'The two passwords differ.';
  return '';
});

async function chooseGame() {
  game.error = null;
  const dir = await chooseFolder('The folder with "Future Soldier DX11.exe"');
  if (!dir) return;
  try {
    game.dir = await call('check_game', { dir });
  } catch (e) {
    game.error = String(e);
  }
}

async function checkServer() {
  if (!server.host) return;
  server.checking = true;
  server.error = null;
  try {
    server.ok = await call('check_server', { server: server.host });
  } catch (e) {
    server.ok = null;
    server.error = String(e);
  } finally {
    server.checking = false;
  }
}

async function run() {
  busy.value = true;
  error.value = null;
  try {
    const said = await call('setup', {
      setup: {
        game_dir: game.dir, server: server.host, name: account.name, password: account.password,
        new_account: account.new, code: account.code, skip_intro: options.skipIntro, dx9: options.dx9,
      },
    });
    emit('finished', said);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  if (!game.dir) {
    game.finding = true;
    game.dir = await call('find_game');
    game.finding = false;
  }
  if (server.host) checkServer();
});
</script>

<style scoped>
.lead { max-width: 640px; margin: 0 0 22px; }
.steps { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 14px; }
.card { position: relative; border-left: 2px solid var(--control-line); transition: border-color .25s; }
.card.done { border-left-color: var(--accent); }
header { display: flex; align-items: baseline; gap: 10px; margin-bottom: 10px; }
header h2 { margin: 0; font: 700 18px/1 PlexCond, sans-serif; text-transform: uppercase; letter-spacing: .02em; }
.n { font: 11px PlexMono, monospace; color: var(--accent); }
.ok { margin-left: auto; font: 11px PlexMono, monospace; letter-spacing: .12em; text-transform: uppercase; color: var(--accent); }
.path { font: 12.5px PlexMono, monospace; color: var(--soft); word-break: break-all; margin: 0 0 12px; }
.row { display: flex; gap: 10px; align-items: flex-end; }
.row .btn { margin-bottom: 14px; min-width: 76px; }
.grow { flex: 1; }
.grid { display: grid; grid-template-columns: 1fr 1fr; column-gap: 12px; }
.tabs { display: inline-flex; margin-bottom: 14px; border: 1px solid var(--control-line); border-radius: 4px; overflow: hidden; }
.tabs button { padding: 6px 14px; color: var(--soft); }
.tabs button.on { background: var(--selected); color: var(--accent); }
.check { display: flex; align-items: center; gap: 10px; margin: 6px 0; color: var(--soft); }
.check input { accent-color: var(--accent); width: 16px; height: 16px; }
.small { font-size: 13px; margin: 0; }
.go { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-top: 20px; }
.go p { margin: 0; }
.big { height: 46px; padding: 0 26px; font-size: 16px; letter-spacing: .02em; }
p { margin: 0 0 10px; }
</style>
