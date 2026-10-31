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
        <div class="card">
          <p class="caps">Server</p>
          <p class="value">{{ info.server }}</p>
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

      <p class="path muted" :title="info.game_dir">{{ info.game_dir }}</p>
    </div>
  </section>
</template>

<script setup>
// The main screen once set up: where you'll play and as whom, and Play, which brings the client
// up to date first when this launcher carries a newer one.
import { computed, ref } from 'vue';
import { call } from '../lib/tauri.js';

const props = defineProps({ info: { type: Object, required: true } });
const emit = defineEmits(['played']);

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
.launch { display: flex; align-items: center; gap: 22px; margin-top: 30px; }
.deploy { height: 60px; padding: 0 44px; display: inline-flex; align-items: center; gap: 12px; font: 700 22px PlexCond, sans-serif; letter-spacing: .08em; text-transform: uppercase; box-shadow: 0 0 0 0 rgba(95, 208, 230, .45); animation: pulse 2.8s ease-in-out infinite; }
.deploy svg { width: 20px; height: 20px; fill: currentColor; stroke: none; }
.deploy:disabled { animation: none; }
.notes p { margin: 0 0 4px; }
.path { margin-top: 34px; font: 11.5px PlexMono, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
@keyframes pulse { 50% { box-shadow: 0 0 0 10px rgba(95, 208, 230, 0); } 0%, 100% { box-shadow: 0 0 0 0 rgba(95, 208, 230, .35); } }
@keyframes rise { from { opacity: 0; transform: translateY(8px) scale(.96); } }
</style>
