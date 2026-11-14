<template>
  <section class="page">
    <div class="inner">
      <p class="kicker">Settings</p>
      <h1 class="title">Settings</h1>

      <div class="card">
        <h2>Game</h2>
        <label class="check"><input v-model="skipIntro" type="checkbox" @change="save" /> Skip the intro videos</label>
        <label class="check"><input v-model="dx9" type="checkbox" @change="save" /> Use DirectX 9 (for older graphics cards)</label>
        <label class="check"><input v-model="overlay" type="checkbox" @change="save" /> In-game overlay</label>
        <label class="check key" :class="{ off: !overlay }">
          Opens with
          <select v-model="overlayKey" :disabled="!overlay" @change="save">
            <option v-for="k in KEYS" :key="k" :value="k">{{ k }}</option>
          </select>
        </label>
        <p v-if="saved" class="muted small">Saved.</p>
        <p v-if="saveError" class="bad small">{{ saveError }}</p>
      </div>

      <div class="card">
        <h2>Account</h2>
        <p class="soft">{{ info.account ? `Signed in as ${info.account}` : 'No account' }} on {{ info.server || 'no server' }}.</p>
        <template v-if="info.server && info.game_dir">
          <div class="tabs" role="tablist">
            <button type="button" role="tab" :aria-selected="!account.new" :class="{ on: !account.new }" @click="account.new = false">Sign in</button>
            <button type="button" role="tab" :aria-selected="account.new" :class="{ on: account.new }" @click="account.new = true">Make an account</button>
          </div>
          <div class="grid">
            <label class="field">
              <span class="caps">Name</span>
              <input v-model.trim="account.name" maxlength="32" spellcheck="false" autocomplete="username" />
            </label>
            <label class="field">
              <span class="caps">Password</span>
              <input v-model="account.password" type="password" maxlength="79" :autocomplete="account.new ? 'new-password' : 'current-password'" @keyup.enter="!account.new && accountReady && setAccount()" />
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
          <div class="row">
            <button type="button" class="btn primary" :disabled="!accountReady || account.busy" @click="setAccount">
              <i v-if="account.busy" class="spin"></i>
              {{ account.new ? 'Make the account' : 'Sign in' }}
            </button>
            <button type="button" class="btn" @click="$emit('go', 'setup')">Change server</button>
          </div>
          <p v-if="account.said" class="small" :class="account.failed ? 'bad' : 'good'">{{ account.said }}</p>
        </template>
        <button v-else type="button" class="btn" @click="$emit('go', 'setup')">Set up a server and account</button>
      </div>

      <div class="card">
        <h2>The game's files</h2>
        <p class="path">{{ info.game_dir || 'No game set up.' }}</p>
        <p class="soft">Cross-Com replaces two of the game's files and keeps the originals. Putting them back leaves the game as it was installed.</p>
        <button v-if="!confirming" type="button" class="btn" :disabled="!info.game_dir || info.client === 'no'" @click="confirming = true">Restore the game's own files</button>
        <div v-else class="row">
          <button type="button" class="btn primary" @click="restore">Restore them</button>
          <button type="button" class="btn" @click="confirming = false">Keep Cross-Com</button>
        </div>
        <p v-if="said" :class="{ bad: failed }">{{ said }}</p>
      </div>

      <p class="muted about">Cross-Com Enhanced {{ info.release }} · an open-source community server for Ghost Recon: Future Soldier.</p>
    </div>
  </section>
</template>

<script setup>
// The game options (with the in-game overlay and its key), signing in to another account or
// making one on the same server, changing server, and taking Cross-Com out of the game.
import { computed, reactive, ref } from 'vue';
import { call } from '../lib/tauri.js';

const props = defineProps({ info: { type: Object, required: true } });
const emit = defineEmits(['go', 'changed']);

const KEYS = Array.from({ length: 12 }, (_, i) => `F${i + 1}`);
const NAME = /^[A-Za-z0-9._-]{1,32}$/;

const skipIntro = ref(props.info.skip_intro);
const dx9 = ref(props.info.dx9);
const overlay = ref(props.info.overlay ?? true);
const overlayKey = ref(props.info.overlay_key || 'F5');
const saved = ref(false);
const saveError = ref(null);
const confirming = ref(false);
const said = ref(null);
const failed = ref(false);
const account = reactive({ new: false, name: '', password: '', again: '', code: '', busy: false, said: null, failed: false });
const accountReady = computed(() => NAME.test(account.name) && account.password && (!account.new || account.password === account.again));

async function save() {
  try {
    await call('set_options', { options: { skip_intro: skipIntro.value, dx9: dx9.value, overlay: overlay.value, overlay_key: overlayKey.value } });
    saved.value = true;
    saveError.value = null;
  } catch (e) {
    saved.value = false;
    saveError.value = String(e);
  }
  emit('changed');
}

async function setAccount() {
  account.busy = true;
  account.said = null;
  try {
    account.said = await call('set_account', {
      account: { name: account.name, password: account.password, new_account: account.new, code: account.code },
    });
    account.failed = false;
    account.password = '';
    account.again = '';
    emit('changed');
  } catch (e) {
    account.said = String(e);
    account.failed = true;
  } finally {
    account.busy = false;
  }
}

async function restore() {
  confirming.value = false;
  try {
    said.value = await call('restore');
    failed.value = false;
  } catch (e) {
    said.value = String(e);
    failed.value = true;
  }
  emit('changed');
}
</script>

<style scoped>
.card { margin-bottom: 14px; max-width: 640px; }
h2 { margin: 0 0 10px; font: 700 18px/1 PlexCond, sans-serif; text-transform: uppercase; letter-spacing: .02em; }
.check { display: flex; align-items: center; gap: 10px; margin: 6px 0; color: var(--soft); }
.check input { accent-color: var(--accent); width: 16px; height: 16px; }
.path { font: 12.5px PlexMono, monospace; color: var(--soft); word-break: break-all; }
.row { display: flex; gap: 10px; align-items: center; }
.key { margin-left: 26px; }
.key.off { opacity: .5; }
.key select { height: 30px; padding: 0 8px; border-radius: 4px; font: 13px PlexMono, monospace; color: var(--fg); background: var(--sunken); border: 1px solid var(--control-line); }
.key select:focus { outline: none; border-color: var(--accent); }
.grid { display: grid; grid-template-columns: 1fr 1fr; column-gap: 12px; }
.tabs { display: inline-flex; margin-bottom: 14px; border: 1px solid var(--control-line); border-radius: 4px; overflow: hidden; }
.tabs button { padding: 6px 14px; color: var(--soft); }
.tabs button.on { background: var(--selected); color: var(--accent); }
.small { font-size: 13px; margin: 8px 0 0; }
p { margin: 0 0 10px; }
.about { font-size: 13px; margin-top: 18px; }
</style>
