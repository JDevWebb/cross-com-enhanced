<template>
  <section class="page">
    <div class="inner">
      <p class="kicker">Settings</p>
      <h1 class="title">Settings</h1>

      <div class="card">
        <h2>Game</h2>
        <label class="check"><input v-model="skipIntro" type="checkbox" @change="save" /> Skip the intro videos</label>
        <label class="check"><input v-model="dx9" type="checkbox" @change="save" /> Use DirectX 9 (for older graphics cards)</label>
        <p v-if="saved" class="muted small">Saved.</p>
      </div>

      <div class="card">
        <h2>Server and account</h2>
        <p class="soft">{{ info.account || 'No account' }} on {{ info.server || 'no server' }}.</p>
        <button type="button" class="btn" @click="$emit('go', 'setup')">Change server or account</button>
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
// The game options, changing server or account, and taking Cross-Com out of the game.
import { ref } from 'vue';
import { call } from '../lib/tauri.js';

const props = defineProps({ info: { type: Object, required: true } });
const emit = defineEmits(['go', 'changed']);

const skipIntro = ref(props.info.skip_intro);
const dx9 = ref(props.info.dx9);
const saved = ref(false);
const confirming = ref(false);
const said = ref(null);
const failed = ref(false);

async function save() {
  await call('set_options', { skipIntro: skipIntro.value, dx9: dx9.value });
  saved.value = true;
  emit('changed');
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
.row { display: flex; gap: 10px; }
.small { font-size: 13px; margin: 6px 0 0; }
p { margin: 0 0 10px; }
.about { font-size: 13px; margin-top: 18px; }
</style>
