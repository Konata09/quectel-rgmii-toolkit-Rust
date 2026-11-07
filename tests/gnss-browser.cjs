// GNSS timing page against the --mock server: menu entries, the ChroGPS Dash frame with live
// data and graphs, separate dashboard settings, appearance, and the not-installed notice.
// Set GNSS_BASE_URL to use a mock server that is already running.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const net = require('node:net');
const {spawn} = require('node:child_process');
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
async function freePort() {
  const server = net.createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
(async () => {
  let base = process.env.GNSS_BASE_URL, child;
  if (!base) {
    const root = path.resolve(__dirname, '..');
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'simpleadmin-gnss-'));
    const port = await freePort(); base = 'http://127.0.0.1:' + port;
    child = spawn(path.join(root, 'target/debug/simpleadmin-httpd'), ['--mock', '--http', '127.0.0.1:' + port, '--static', path.join(root, 'development/simpleadmin/www'), '--auth-file', path.join(temporary, 'auth'), '--ttl-file', path.join(temporary, 'ttl')], {stdio: 'ignore'});
    for (let i = 0; i < 100; i++) { try { await fetch(base + '/login.html'); break; } catch { await new Promise(r => setTimeout(r, 100)); } }
  }
  const browser = await chromium.launch({headless: true, executablePath: process.env.CHROMIUM_EXECUTABLE_PATH || undefined});
  try {
    const context = await browser.newContext({viewport: {width: 1280, height: 900}, locale: 'zh-CN'});
    const page = await context.newPage();
    const errors = []; page.on('pageerror', e => errors.push(e.message));
    await page.goto(base + '/login.html');
    await page.locator('#loginLanguage').selectOption('zh-CN');
    await page.locator('#username').fill('admin'); await page.locator('#password').fill('admin');
    await page.locator('#loginButton').click(); await page.waitForURL(base + '/');

    // --- Menu entry appears once the server reports chrony; the frame loads on the first visit ---
    const link = page.locator('.sa-sidebar [data-page-link="gnss"]');
    await link.waitFor({state: 'visible'});
    assert.equal(await page.locator('#gnssFrame').getAttribute('src'), null, 'not loaded before a visit');
    await link.click();
    await page.waitForFunction(() => location.hash === '#gnss');
    assert.equal(await page.locator('#page-title-gnss').textContent(), 'GNSS 授时');
    assert.equal(await page.locator('#gnssFrame').getAttribute('src'), '/gnss/');
    // The frame fills everything beside the sidebar: no title, footer or margins.
    const fills = () => page.evaluate(() => {
      const r = document.getElementById('gnssFrame').getBoundingClientRect();
      const side = document.querySelector('.sa-sidebar').getBoundingClientRect();
      const top = document.querySelector('.sa-topbar').getBoundingClientRect();
      const tabs = document.querySelector('.sa-tabbar').getBoundingClientRect();
      const phone = getComputedStyle(document.querySelector('.sa-sidebar')).display === 'none';
      const want = phone
        ? {left: 0, top: top.bottom, right: innerWidth, bottom: tabs.top}
        : {left: side.right, top: 0, right: innerWidth, bottom: innerHeight};
      const off = Object.keys(want).filter(k => Math.abs(r[k] - want[k]) > 1).map(k => `${k} ${r[k]} != ${want[k]}`);
      return {off, scroll: document.scrollingElement.scrollHeight - innerHeight};
    });
    assert.deepEqual(await fills(), {off: [], scroll: 0});
    assert.equal(await page.locator('#page-title-gnss').isVisible(), false);
    assert.equal(await page.locator('.sa-site-footer').isVisible(), false);
    const frame = page.frameLocator('#gnssFrame');
    const inside = () => page.frames().find(f => f.url().endsWith('/gnss/'));
    await page.waitForFunction(() => document.getElementById('gnssFrame').contentDocument?.readyState === 'complete');
    const dash = inside();

    // --- Live data from /api/gnss: node name, sync state, sources, satellites ---
    await frame.locator('.header-status').filter({hasText: 'NTP Node: "RG520N-EB"'}).waitFor({timeout: 20000});
    assert.match(await dash.title(), /^RG520N-EB - ChroGPS Dash$/);
    await dash.waitForFunction(() => document.getElementById('sources-body')?.textContent.includes('PPS'), null, {timeout: 20000});
    const lock = await frame.locator('#lock-badge-text').textContent();
    assert.notEqual(lock.trim(), 'NO SYNC', lock);
    await dash.waitForFunction(() => Number(document.getElementById('sat-cnt-total')?.textContent) > 0, null, {timeout: 20000});

    // --- Graphs drawn in the browser from the backfilled preview history ---
    await dash.waitForFunction(() => document.querySelectorAll('#graph-tracking svg, #graph-pps svg').length > 0, null, {timeout: 20000});
    const graphs = await dash.evaluate(() => ['tracking', 'stats', 'frequency', 'pps', 'satcounts', 'snr', 'dop', 'rootdelaydisp']
      .filter(id => document.querySelector(`#graph-${id} svg`)));
    assert(graphs.length >= 6, 'graphs: ' + graphs.join(','));
    await frame.locator('.history-btn[data-hours="24"]').click();
    await dash.waitForFunction(() => document.querySelector('.history-btn[data-hours="24"]').classList.contains('active'));
    await dash.waitForFunction(() => document.querySelector('#graph-snr-constellation svg'), null, {timeout: 20000});

    // --- The clients list reads the chronyc clients part ---
    const clients = await dash.evaluate(() => fetch('?ajax=clients').then(r => r.json()));
    assert.equal(clients.clients.length, 3);

    // --- Dashboard settings stay apart from SimpleAdmin's, which share this origin ---
    await page.locator('.sa-sidebar [data-theme-select]').selectOption('dark');
    assert.equal(await dash.evaluate(() => document.documentElement.getAttribute('data-theme')), 'dark', 'follows the appearance');
    await dash.evaluate(() => localStorage.setItem('theme', 'dracula'));
    assert.equal(await page.evaluate(() => localStorage.getItem('theme')), 'dark', 'SimpleAdmin keeps its choice');
    assert.equal(await page.evaluate(() => localStorage.getItem('gnss.theme')), 'dracula');
    assert.equal(await dash.evaluate(() => localStorage.getItem('theme')), 'dracula');
    await page.locator('.sa-sidebar [data-theme-select]').selectOption('light');
    assert.equal(await dash.evaluate(() => document.documentElement.getAttribute('data-theme')), 'dark', 'a dashboard theme wins once picked');
    await page.reload();
    assert.equal(await page.evaluate(() => document.documentElement.getAttribute('data-bs-theme')), 'light');

    // --- Phone: the entry sits in the More sheet ---
    await page.setViewportSize({width: 390, height: 844});
    await page.goto(base + '/#dashboard');
    await page.locator('.sa-tabbar [data-tab="more"]').click();
    const row = page.locator('#moreSheet [data-tab="gnss"]');
    await row.waitFor({state: 'visible'});
    await row.click();
    await page.waitForFunction(() => location.hash === '#gnss');
    assert(await page.locator('#gnssPanel').isVisible());
    assert.equal(await page.locator('.sa-tabbar [data-tab="more"]').getAttribute('aria-current'), 'page');
    assert.deepEqual(await fills(), {off: [], scroll: 0});
    // Leaving the page brings the title and footer back elsewhere.
    await page.goto(base + '/#deviceinfo');
    assert.equal(await page.evaluate(() => document.documentElement.classList.contains('sa-gnss-full')), false);
    assert(await page.locator('.sa-site-footer').isVisible());

    // --- English ---
    await page.setViewportSize({width: 1280, height: 900});
    await page.locator('.sa-sidebar [data-language-select]').selectOption('en');
    await page.waitForFunction(() => document.getElementById('page-title-gnss').textContent === 'GNSS timing');

    // --- Without chrony the entries stay hidden and the address shows a notice ---
    const bare = await context.newPage();
    await bare.route('**/api/gnss', route => route.fulfill({status: 200, contentType: 'application/json', body: '{"installed":false}'}));
    await bare.goto(base + '/#gnss');
    await bare.locator('#gnssMissing').waitFor({state: 'visible'});
    assert(await bare.locator('#gnssMissing').textContent().then(t => t.includes('chrony')));
    assert.equal(await bare.locator('.sa-sidebar [data-page-link="gnss"]').isVisible(), false);
    assert.equal(await bare.locator('#gnssPanel').isVisible(), false);
    assert.equal(await bare.locator('#gnssFrame').getAttribute('src'), null);

    // --- A lost session sends the whole window to the login page ---
    await page.goto(base + '/#gnss');
    await page.locator('#gnssFrame').waitFor();
    await page.waitForFunction(() => document.getElementById('gnssFrame').contentDocument?.readyState === 'complete');
    await context.clearCookies();
    await page.waitForURL(/\/login\.html$/, {timeout: 30000});

    assert.deepEqual(errors, []);
    console.log('gnss browser test passed');
  } finally {
    await browser.close();
    if (child) child.kill();
  }
})().catch(e => { console.error(e); process.exit(1); });
