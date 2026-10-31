<template>
  <nav class="rail" aria-label="Launcher">
    <button
      v-for="item in items"
      :key="item.id"
      type="button"
      :class="{ on: current === item.id, low: item.low }"
      :aria-label="item.label"
      :aria-current="current === item.id ? 'page' : null"
      :title="item.label"
      @click="$emit('go', item.id)"
    >
      <svg viewBox="0 0 24 24" v-html="item.icon"></svg>
    </button>
  </nav>
</template>

<script setup>
// The side menu: the screens as icons, named on hover; Settings at the foot.
defineProps({ current: String });
defineEmits(['go']);

const items = [
  { id: 'play', label: 'Play', icon: '<path d="M7 4.5v15l12-7.5z"/>' },
  { id: 'setup', label: 'Set up: game, server and account', icon: '<path d="M4 7h10M4 17h6M18 7h2M14 17h6"/><circle cx="16" cy="7" r="2"/><circle cx="12" cy="17" r="2"/>' },
  { id: 'settings', label: 'Settings', low: true, icon: '<circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M5.6 18.4l2.1-2.1M16.3 7.7l2.1-2.1"/>' },
];
</script>

<style scoped>
.rail { width: 60px; flex: none; display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 16px 0 12px; background: var(--rail); border-right: 1px solid var(--line); }
button { position: relative; width: 42px; height: 42px; border-radius: 8px; display: grid; place-items: center; color: var(--muted); transition: background .2s, color .2s; }
button.low { margin-top: auto; }
button svg { width: 20px; height: 20px; }
button:hover { background: var(--control); color: var(--fg); }
button.on { background: var(--selected); color: var(--accent); }
button.on::before { content: ""; position: absolute; left: 0; top: 8px; bottom: 8px; width: 2px; border-radius: 1px; background: var(--accent); animation: grow .35s var(--ease); }
@keyframes grow { from { transform: scaleY(0); } }
</style>
