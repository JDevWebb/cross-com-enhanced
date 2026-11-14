<template>
  <section class="page play">
    <div class="inner">
      <div class="hero">
        <img class="logo" src="/logo.svg" alt="" />
        <div>
          <p class="kicker">Ghost Recon: Future Soldier</p>
          <h1 class="title">Ready for deployment</h1>
          <p class="soft">Multiplayer on a Cross-Com server: Conflict, Decoy, Saboteur, Siege, Guerrilla and co-op.</p>
        </div>
      </div>

      <div class="status">
        <div class="card server">
          <p class="caps">
            Server
            <span class="state" :class="state.kind"><i></i>{{ state.label }}</span>
          </p>
          <p class="value">{{ info.server }}</p>
          <p class="muted small">{{ state.detail }}</p>
        </div>
        <div class="card">
          <p class="caps">Signed in as</p>
          <p class="value">{{ info.account }}</p>
        </div>
        <div class="card">
          <p class="caps">Client</p>
          <p class="value" :class="{ warn: info.client === 'update' }">{{ clientText }}</p>
        </div>
      </div>

      <div class="launch">
        <button type="button" class="btn primary deploy" :disabled="starting" @click="play">
          <svg viewBox="0 0 24 24"><path d="M7 4.5v15l12-7.5z" /></svg>
          {{ starting ? 'Starting…' : 'Play' }}
        </button>
        <div class="notes">
          <p class="muted">{{ info.dx9 ? 'DirectX 9' : 'DirectX 11' }}<span v-if="info.skip_intro"> · intro videos skipped</span></p>
          <p v-if="said" :class="{ bad: failed }">{{ said }}</p>
        </div>
      </div>

      <section class="news" aria-labelledby="news-title">
        <header>
          <h2 id="news-title">News</h2>
          <button type="button" class="refresh" :disabled="status.loading" title="Ask the server again" aria-label="Refresh" @click="load">
            <svg viewBox="0 0 24 24" :class="{ turning: status.loading }"><path d="M20 12a8 8 0 1 1-2.3-5.6M20 4v4.5h-4.5" /></svg>
          </button>
        </header>
        <TransitionGroup v-if="news.length" name="item" tag="ol">
          <li v-for="(item, i) in news" :key="item.date + item.title + i" class="card item">
            <p class="meta"><span class="caps">{{ item.date }}</span><span v-if="item.shows" class="tag">{{ shows[item.shows] || item.shows }}</span></p>
            <h3>{{ item.title }}</h3>
            <p class="soft">{{ item.body }}</p>
          </li>
        </TransitionGroup>
        <p v-else class="muted">{{ newsNote }}</p>
      </section>

      <p class="path muted" :title="info.game_dir">{{ info.game_dir }}</p>
    </div>
  </section>
</template>

<script setup>
// The main screen once set up: where you'll play and as whom, how the server is (asked each time
// the screen opens), its news, and Play, which brings the client up to date first when this
// launcher carries a newer one.
import { computed, onMounted, reactive, ref } from 'vue';
import { call } from '../lib/tauri.js';

const props = defineProps({ info: { type: Object, required: true } });
const emit = defineEmits(['played']);

// What the server said: its info, null for a server older than the launcher's API, or an error.
const status = reactive({ loading: true, info: undefined, error: null });
const news = computed(() => status.info?.news ?? []);
const newsNote = computed(() => {
  if (status.loading) return 'Asking the server…';
  if (status.error) return 'The server didn’t answer, so there’s no news.';
  if (status.info === null) return 'This server doesn’t share its news with the launcher.';
  return 'No news from the server.';
});
const shows = { multiplayer: 'Multiplayer', campaign: 'Campaign', guerrilla: 'Guerrilla' };
const state = computed(() => {
  if (status.loading && status.info === undefined) return { kind: 'asking', label: 'Checking', detail: 'Asking the server…' };
  if (status.error) return { kind: 'down', label: 'Offline', detail: status.error };
  if (!status.info) return { kind: 'up', label: 'Online', detail: 'An older server: no player count or news.' };
  const n = status.info.players_online;
  return { kind: 'up', label: 'Online', detail: `${n === 1 ? '1 player' : `${n} players`} online · server ${status.info.version}` };
});

async function load() {
  status.loading = true;
  try {
    status.info = await call('server_info');
    status.error = null;
  } catch (e) {
    status.info = undefined;
    status.error = String(e);
  } finally {
    status.loading = false;
  }
}

onMounted(load);

const starting = ref(false);
const said = ref(null);
const failed = ref(false);
const clientText = computed(() => ({ yes: 'Installed', update: 'Update ready: installed when you press Play', no: 'Not installed' })[props.info.client] || '—');

async function play() {
  starting.value = true;
  failed.value = false;
  try {
    said.value = await call('play');
  } catch (e) {
    said.value = String(e);
    failed.value = true;
  } finally {
    starting.value = false;
    emit('played');
  }
}
</script>

<style scoped>
.play { background: radial-gradient(1100px 520px at 85% -10%, rgba(95, 208, 230, .10), transparent 60%), var(--bg); }
.hero { display: flex; align-items: center; gap: 26px; margin: 18px 0 30px; }
.logo { width: 112px; height: 112px; filter: drop-shadow(0 0 22px rgba(95, 208, 230, .30)); animation: rise .6s var(--ease); }
.hero p { margin: 0; }
.title { font-size: 52px; }
.status { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px; }
.status .card { padding: 14px 18px; }
.status p { margin: 0; }
.value { margin-top: 6px !important; font: 600 16px Plex, sans-serif; word-break: break-all; }
.value.warn { color: var(--warn); font-size: 14px; }
.small { font-size: 12.5px; margin-top: 4px !important; overflow-wrap: anywhere; }
.server .caps { display: flex; align-items: center; }
.state { margin-left: auto; display: inline-flex; align-items: center; gap: 6px; letter-spacing: .12em; }
.state i { width: 7px; height: 7px; border-radius: 50%; background: var(--muted); }
.state.up { color: var(--good); } .state.up i { background: var(--good); box-shadow: 0 0 8px var(--good); }
.state.down { color: var(--bad); } .state.down i { background: var(--bad); }
.state.asking i { animation: blink 1s ease-in-out infinite; }
.news { margin-top: 34px; }
.news header { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
.news h2 { margin: 0; font: 700 18px/1 PlexCond, sans-serif; text-transform: uppercase; letter-spacing: .02em; }
.refresh { width: 30px; height: 30px; border-radius: 6px; display: grid; place-items: center; color: var(--muted); }
.refresh:hover:not(:disabled) { background: var(--control); color: var(--fg); }
.refresh svg { width: 16px; height: 16px; }
.turning { animation: spin .8s linear infinite; }
.news ol { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
.item { padding: 14px 18px; border-left: 2px solid var(--accent-dim); }
.item h3 { margin: 4px 0 4px; font: 600 16px Plex, sans-serif; }
.item p { margin: 0; white-space: pre-line; }
.meta { display: flex; align-items: center; gap: 10px; }
.tag { font: 10.5px PlexMono, monospace; letter-spacing: .12em; text-transform: uppercase; color: var(--accent); border: 1px solid var(--accent-dim); border-radius: 3px; padding: 1px 6px; }
.item-enter-active { transition: opacity .3s, transform .35s var(--ease); }
.item-enter-from { opacity: 0; transform: translateY(6px); }
@keyframes blink { 50% { opacity: .3; } }
.launch { display: flex; align-items: center; gap: 22px; margin-top: 30px; }
.deploy { height: 60px; padding: 0 44px; display: inline-flex; align-items: center; gap: 12px; font: 700 22px PlexCond, sans-serif; letter-spacing: .08em; text-transform: uppercase; box-shadow: 0 0 0 0 rgba(95, 208, 230, .45); animation: pulse 2.8s ease-in-out infinite; }
.deploy svg { width: 20px; height: 20px; fill: currentColor; stroke: none; }
.deploy:disabled { animation: none; }
.notes p { margin: 0 0 4px; }
.path { margin-top: 34px; font: 11.5px PlexMono, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
@keyframes pulse { 50% { box-shadow: 0 0 0 10px rgba(95, 208, 230, 0); } 0%, 100% { box-shadow: 0 0 0 0 rgba(95, 208, 230, .35); } }
@keyframes rise { from { opacity: 0; transform: translateY(8px) scale(.96); } }
</style>
