// GNSS timing: the ChroGPS Dash page (www/gnss/) in a frame. Its menu entries stay hidden
// unless the server reports chrony installed, and the frame loads on the first visit.
(function (global) {
  'use strict';

  const root = global.SimpleAdmin || (global.SimpleAdmin = {});
  // null until the server answers; then whether chronyc is installed.
  let installed = null;

  function frame() {
    return document.getElementById('gnssFrame');
  }

  function render() {
    document.querySelectorAll('[data-gnss-link]').forEach((element) => {
      element.hidden = installed !== true;
    });
    const missing = document.getElementById('gnssMissing');
    const panel = document.getElementById('gnssPanel');
    if (missing) missing.hidden = installed !== false;
    if (panel) panel.hidden = installed !== true;
    const visible = root.Spa && typeof root.Spa.current === 'function' && root.Spa.current() === 'gnss';
    // The frame fills the content area: no title, footer or margins.
    document.documentElement.classList.toggle('sa-gnss-full', visible && installed === true);
    measureBars();
    const element = frame();
    if (visible && installed === true && element && !element.getAttribute('src')) {
      element.setAttribute('src', '/gnss/');
    }
  }

  // On phones the frame sits between the sticky top bar and the fixed tab bar.
  function measureBars() {
    const html = document.documentElement;
    if (!html.classList.contains('sa-gnss-full')) return;
    const top = document.querySelector('.sa-topbar');
    const bottom = document.querySelector('.sa-tabbar');
    html.style.setProperty('--sa-gnss-top', `${top ? top.getBoundingClientRect().height : 0}px`);
    html.style.setProperty('--sa-gnss-bottom', `${bottom ? bottom.getBoundingClientRect().height : 0}px`);
  }

  async function check() {
    try {
      const response = await global.fetch('/api/gnss', { cache: 'no-store', credentials: 'same-origin' });
      if (response.ok) installed = (await response.json()).installed === true;
    } catch (_) {
      // Keep the entries hidden; the next page load asks again.
    }
    render();
  }

  // The dashboard follows SimpleAdmin's appearance (shim.js reads its choice) until a
  // dashboard theme is picked inside the frame; pass later changes on too.
  function followTheme(theme) {
    const element = frame();
    if (!theme || !element) return;
    try {
      if (global.localStorage.getItem('gnss.theme') !== null) return;
      const doc = element.contentDocument;
      if (doc && doc.documentElement) doc.documentElement.setAttribute('data-theme', theme);
    } catch (_) {
      // Storage or the frame is unavailable; the dashboard keeps its own theme.
    }
  }

  global.addEventListener('simpleadmin:page-changed', render);
  global.addEventListener('resize', measureBars);
  global.addEventListener('simpleadmin:theme-changed', (event) => {
    followTheme(event.detail && event.detail.theme);
  });

  root.Gnss = { check, installed: () => installed };
  check();
})(window);
