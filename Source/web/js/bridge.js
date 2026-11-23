// In the Rust (Tauri) version, the page's App.api.<name>(...) calls go to Rust instead of Python,
// and Rust's live events reach window.onCore just like before. Does nothing in the Python version.
(() => {
  const T = window.__TAURI__;
  if (!T || window.pywebview) return;
  const api = new Proxy({}, {
    get: (_, method) => typeof method === 'string'
      ? (...args) => T.core.invoke('call', { method, args })
      : undefined,
  });
  window.pywebview = { api };
  T.event.listen('core', e => window.onCore && window.onCore(e.payload.event, e.payload.data));
  window.addEventListener('DOMContentLoaded', () => window.dispatchEvent(new Event('pywebviewready')));
})();
