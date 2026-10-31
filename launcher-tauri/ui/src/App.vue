<template>
  <div class="window" :class="{ framed: !maximised }">
    <TitleBar :product="info.product" :release="info.release" />
    <div class="body">
      <Rail :current="shown" @go="go" />
      <main class="screen">
        <Transition name="view" mode="out-in">
          <div v-if="!loaded" key="loading" class="page loading"><i class="spin"></i></div>
          <Setup v-else-if="shown === 'setup'" :key="'setup' + setupRun" :info="info" :first="!info.ready" @finished="finished" @cancel="go('play')" />
          <Play v-else-if="shown === 'play'" key="play" :info="info" @played="refresh" />
          <Settings v-else key="settings" :info="info" @go="go" @changed="refresh" />
        </Transition>
      </main>
    </div>
    <Transition name="toast">
      <div v-if="toast" class="toast" role="status">{{ toast }}</div>
    </Transition>
  </div>
</template>

<script setup>
// The launcher's window: its own title bar, the side menu, and the screen chosen. Until the game,
// a server and an account are set up, the guided setup is the screen.
import { computed, onMounted, reactive, ref } from 'vue';
import TitleBar from './components/TitleBar.vue';
import Rail from './components/Rail.vue';
import Setup from './views/Setup.vue';
import Play from './views/Play.vue';
import Settings from './views/Settings.vue';
import { call, inTauri } from './lib/tauri.js';

const info = reactive({ product: 'Cross-Com Enhanced', release: '', ready: false });
const loaded = ref(false);
const screen = ref('play');
const setupRun = ref(0);
const maximised = ref(false);
const toast = ref(null);

// Play needs everything set up: until then it's the setup.
const shown = computed(() => (screen.value === 'play' && !info.ready ? 'setup' : screen.value));

function go(id) {
  if (id === 'setup') setupRun.value += 1;
  screen.value = id;
}

async function refresh() {
  Object.assign(info, await call('info'));
  loaded.value = true;
}

async function finished(said) {
  await refresh();
  screen.value = 'play';
  toast.value = said;
  setTimeout(() => (toast.value = null), 4000);
}

onMounted(async () => {
  await refresh();
  if (inTauri) {
    const w = window.__TAURI__.window.getCurrentWindow();
    const check = async () => (maximised.value = await w.isMaximized());
    check();
    w.onResized(check);
  }
});
</script>

<style scoped>
.window { height: 100%; display: flex; flex-direction: column; }
.window.framed { border: 1px solid var(--control-line); }
.body { flex: 1; min-height: 0; display: flex; }
.screen { flex: 1; min-width: 0; position: relative; }
.screen > * { height: 100%; }
.loading { display: grid; place-items: center; }
.toast { position: fixed; right: 22px; bottom: 22px; padding: 12px 18px; border-radius: 6px; background: var(--surface); border: 1px solid var(--accent-dim); border-left: 3px solid var(--accent); box-shadow: 0 18px 50px rgba(0, 0, 0, .5); }
.toast-enter-active, .toast-leave-active { transition: opacity .25s, transform .3s var(--ease); }
.toast-enter-from, .toast-leave-to { opacity: 0; transform: translateY(8px); }
</style>
