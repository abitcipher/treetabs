// Loads the tree window (wasm).
import init from './ui/treetabs_extension_ui.js';
init();

// Remember where the tree window is and how big it is: the toolbar button
// opens it there next time (see open_tree_window in the extension).
const api = globalThis.browser ?? chrome;
let last = '';
async function saveBounds() {
  try {
    const w = await api.windows.getCurrent();
    if (w.state !== 'normal') return; // keep the normal size, not maximized
    const b = { left: w.left, top: w.top, width: w.width, height: w.height };
    const key = JSON.stringify(b);
    if (key !== last) {
      last = key;
      await api.storage.local.set({ treeBounds: b });
    }
  } catch (e) {
    console.warn('[treetabs] cannot save the window bounds', e);
  }
}
// Moving a window fires no page event, so look once a second as well.
addEventListener('resize', saveBounds);
setInterval(saveBounds, 1000);
