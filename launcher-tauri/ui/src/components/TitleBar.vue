<template>
  <header class="titlebar" data-tauri-drag-region @dblclick.self="win.toggleMaximize()">
    <img class="mark" src="/logo.svg" alt="" data-tauri-drag-region />
    <span class="product" data-tauri-drag-region>{{ product.toUpperCase() }} <span class="release">{{ release }}</span></span>
    <span class="spacer" data-tauri-drag-region @dblclick="win.toggleMaximize()"></span>
    <button class="wc" type="button" aria-label="Minimise" title="Minimise" @click="win.minimize()"><svg viewBox="0 0 12 12"><path d="M1 6.5h10" /></svg></button>
    <button class="wc" type="button" aria-label="Maximise" title="Maximise" @click="win.toggleMaximize()"><svg viewBox="0 0 12 12"><rect x="1.5" y="1.5" width="9" height="9" /></svg></button>
    <button class="wc close" type="button" aria-label="Close" title="Close" @click="win.close()"><svg viewBox="0 0 12 12"><path d="M1.5 1.5l9 9M10.5 1.5l-9 9" /></svg></button>
  </header>
</template>

<script setup>
// The window's own title bar: dragged to move it, double-clicked to maximise it, its buttons the
// window's (the system's frame is off, tauri.conf.json).
import { win } from '../lib/tauri.js';

defineProps({ product: { type: String, default: '' }, release: { type: String, default: '' } });
</script>

<style scoped>
.titlebar { height: 34px; flex: none; display: flex; align-items: center; background: var(--rail); border-bottom: 1px solid var(--line); }
.mark { width: 18px; height: 18px; margin: 0 10px 0 12px; filter: drop-shadow(0 0 6px rgba(95, 208, 230, .45)); }
.product { font: 11px PlexMono, monospace; letter-spacing: .12em; color: var(--muted); }
.release { color: var(--accent); opacity: .8; }
.spacer { flex: 1; height: 100%; }
.wc { width: 46px; height: 34px; display: grid; place-items: center; color: var(--soft); transition: background .15s, color .15s; }
.wc svg { width: 11px; height: 11px; stroke-width: 1.2; }
.wc:hover { background: var(--control-hover); color: var(--fg); }
.wc.close:hover { background: var(--bad); color: #fff; }
</style>
