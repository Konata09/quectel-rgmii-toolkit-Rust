/*
 * Lets the unmodified ChroGPS Dash front end run without PHP inside SimpleAdmin.
 * Its requests to ?ajax=... are answered from /api/gnss, where the server keeps
 * what it collected in memory, and the graphs PHP used to render on the server
 * are drawn here, so the module does no chart work. The graph functions below are
 * ports of the PHP originals.
 *
 * ChroGPS Dash (c) 2026 Chip Cuccio, MIT License, https://chro.gpsda.sh/license/
 */
(function () {
	'use strict';

	const VERSION = '__VERSION__';
	const realFetch = window.fetch.bind(window);
	const json = (o) => new Response(JSON.stringify(o), { headers: { 'Content-Type': 'application/json' } });
	// live.json -> /api/gnss?part=live. A lost session sends the whole window to the login.
	const data = async (name) => {
		const part = name.replace(/\.\w+$/, '');
		const r = await realFetch(`/api/gnss?part=${part}&t=${Date.now()}`, { cache: 'no-store', credentials: 'same-origin' });
		if (r.status === 401) window.top.location.href = '/login.html';
		return r;
	};

	// This page shares its origin, and so localStorage, with SimpleAdmin, which keeps its
	// own appearance under "theme". Keep the dashboard's settings under a prefix, and follow
	// SimpleAdmin's light or dark choice until a dashboard theme is picked here. Only this
	// frame's Storage prototype changes; SimpleAdmin's page is untouched.
	try {
		const proto = Storage.prototype;
		const get = proto.getItem, set = proto.setItem, remove = proto.removeItem;
		const own = (key) => `gnss.${key}`;
		proto.getItem = function (key) {
			const value = get.call(this, own(key));
			if (value === null && key === 'theme') {
				const shell = get.call(this, 'theme');
				return shell === 'light' || shell === 'dark' ? shell : null;
			}
			return value;
		};
		proto.setItem = function (key, value) { return set.call(this, own(key), value); };
		proto.removeItem = function (key) { return remove.call(this, own(key)); };
	} catch (_) {
		// No storage (private mode): the page keeps its defaults.
	}

	// The module's model, as the server read it, in the header and the tab title. The header
	// styles its spans, so the name goes into the text itself.
	let node = '';
	function showNode(name) {
		if (!name || name === node) return;
		const header = document.querySelector('.header-status');
		const text = header && Array.from(header.childNodes).find((n) => n.nodeType === 3 && n.nodeValue.includes('NTP Node:'));
		if (!text) return;
		node = name;
		text.nodeValue = text.nodeValue.replace(/"[^"]*"/, `"${name}"`);
		document.title = `${name} - ChroGPS Dash`;
	}

	// ---- helpers matching PHP round() and date() output ----

	const R = (v, n = 0) => {
		const m = Math.pow(10, n);
		const r = Math.round(Math.abs(v) * m + 1e-9) / m;
		return String(v < 0 && r !== 0 ? -r : r);
	};
	const hm = (ts) => new Date(ts * 1000).toISOString().substr(11, 5);
	const hms = (ts) => new Date(ts * 1000).toISOString().substr(11, 8);
	const sum = (a) => a.reduce((x, y) => x + y, 0);
	const msg = (t) => `<p class='graph-message'>${t}</p>`;

	const W = 1200, H = 300, PX = 95, PY = 35;

	function autoUnit(unit, absMax) {
		if (unit !== 's') return [unit, 1];
		if (absMax < 0.000001) return ['ns', 1e9];
		if (absMax < 0.001) return ['µs', 1e6];
		if (absMax < 1) return ['ms', 1e3];
		return [unit, 1];
	}

	function drawGraphSVG(d, label1, label2, unit1, unit2, color = 'var(--green)', zeroBase = false, static2 = null) {
		if (!d.length) return msg('No valid data points found.');
		const count = d.length;
		const v1 = d.map((p) => p.val1), v2 = d.map((p) => p.val2);
		const max1 = Math.max(...v1);
		let min1 = Math.min(...v1);
		if (zeroBase) {
			min1 = Math.min(0, min1);
			if (min1 === 0 && max1 > 0) min1 = -(max1 * 0.05);
		}
		const max2 = Math.max(...v2), min2 = Math.min(...v2);
		const [u1, m1] = autoUnit(unit1, Math.max(Math.abs(max1), Math.abs(min1)));
		const [u2, m2] = autoUnit(unit2, Math.max(Math.abs(max2), Math.abs(min2)));
		const range1 = (max1 - min1) || 1e-9, range2 = (max2 - min2) || 1e-9;
		const xStep = count > 1 ? (W - 2 * PX) / (count - 1) : 0;
		const barW = count > 1 ? xStep : W - 2 * PX;

		let pts1 = '', pts2 = '', hover = '';
		d.forEach((p, i) => {
			const x = PX + i * xStep;
			const y1 = (H - PY) - ((p.val1 - min1) / range1 * (H - 2 * PY));
			const y2 = (H - PY) - ((p.val2 - min2) / range2 * (H - 2 * PY));
			pts1 += `${x},${y1} `;
			pts2 += `${x},${y2} `;
			const l2 = static2 === null ? ` data-l2='${label2}' data-v2='${R(p.val2 * m2, 3)}${u2}' data-c2='var(--accent)'` : '';
			hover += `<rect x='${x - barW / 2}' y='${PY}' width='${barW}' height='${H - 2 * PY}' fill='transparent' class='graph-hit-zone' data-time='${p.time} UTC' data-l1='${label1}' data-v1='${R(p.val1 * m1, 3)}${u1}' data-c1='${color}'${l2} />`;
		});

		const bottom = H - PY, lastX = PX + (count - 1) * xStep;
		const fill1 = `M${PX},${bottom} L${pts1.trim().replace(/ /g, ' L')} L${lastX},${bottom} Z`;
		let svg = `<svg viewBox='0 0 ${W} ${H}' preserveAspectRatio='xMidYMid meet' class='graph-svg'>`;
		svg += `<path d='${fill1}' fill='${color}' fill-opacity='0.05' stroke='none' />`;

		for (let i = 0; i <= 5; i++) {
			const vx = PX + i * (W - 2 * PX) / 5;
			if (i > 0 && i < 5) {
				svg += `<line x1='${vx}' y1='${PY}' x2='${vx}' y2='${H - PY}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				const hy = PY + i * (H - 2 * PY) / 5;
				svg += `<line x1='${PX}' y1='${hy}' x2='${W - PX}' y2='${hy}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				svg += `<text x='${PX - 10}' y='${hy + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${R((max1 - i * range1 / 5) * m1, 1)}</text>`;
			}
			const idx = Math.max(0, Math.min(Math.trunc((count - 1) * i / 5), count - 1));
			const anchor = i === 0 ? 'start' : (i === 5 ? 'end' : 'middle');
			svg += `<text x='${vx}' y='${H - PY + 25}' fill='var(--text)' font-family='var(--font-mono)' font-size='12' text-anchor='${anchor}'>${d[idx].time} UTC</text>`;
		}

		if ((min1 < 0 && max1 > 0) || (zeroBase && max1 > 0)) {
			const zy = (H - PY) - ((0 - min1) / range1 * (H - 2 * PY));
			svg += `<line x1='${PX}' y1='${zy}' x2='${W - PX}' y2='${zy}' stroke='var(--text)' stroke-opacity='0.5' stroke-width='1' />`;
			svg += `<text x='${PX - 10}' y='${zy + 4}' fill='var(--text)' font-weight='bold' font-family='var(--font-mono)' font-size='12' text-anchor='end'>0</text>`;
		}

		svg += `<rect x='${PX}' y='${PY}' width='${W - 2 * PX}' height='${H - 2 * PY}' fill='none' stroke='var(--border)' stroke-width='1' />`;
		svg += `<text x='${PX - 10}' y='${PY + 4}' fill='${color}' font-family='var(--font-mono)' font-size='12' text-anchor='end'>${R(max1 * m1, 3)}${u1}</text>`;
		svg += `<text x='${PX - 10}' y='${H - PY + 4}' fill='${color}' font-family='var(--font-mono)' font-size='12' text-anchor='end'>${R(min1 * m1, 3)}${u1}</text>`;
		svg += `<text x='20' y='${H / 2}' text-anchor='middle' fill='${color}' font-family='var(--font-sans)' font-size='14' font-weight='bold' transform='rotate(-90, 20, ${H / 2})'>${label1}</text>`;
		if (static2 === null) {
			svg += `<text x='${W - PX + 5}' y='${PY + 4}' fill='var(--accent)' font-family='var(--font-mono)' font-size='12' text-anchor='start'>${R(max2 * m2, 3)}${u2}</text>`;
			svg += `<text x='${W - PX + 5}' y='${H - PY + 4}' fill='var(--accent)' font-family='var(--font-mono)' font-size='12' text-anchor='start'>${R(min2 * m2, 3)}${u2}</text>`;
			svg += `<text x='${W - 20}' y='${H / 2}' text-anchor='middle' fill='var(--accent)' font-family='var(--font-sans)' font-size='14' font-weight='bold' transform='rotate(90, ${W - 20}, ${H / 2})'>${label2}</text>`;
		}
		svg += `<polyline points='${pts1}' fill='none' stroke='${color}' stroke-width='2' stroke-linejoin='round' />`;
		if (static2 === null) svg += `<polyline points='${pts2}' fill='none' stroke='var(--accent)' stroke-width='1' stroke-linejoin='round' opacity='0.8' />`;
		svg += `<g class='hover-layer'>${hover}</g></svg>`;

		const popup = (vals, m, u) => `<div class='popup legend-stats-popup'><span class='op-muted'>avg</span> <span class='font-mono'>${R(sum(vals) / vals.length * m, 3)}${u}</span><br><span class='op-muted'>range</span> <span class='font-mono'>${R(Math.min(...vals) * m, 3)}-${R(Math.max(...vals) * m, 3)}${u}</span></div>`;
		let legend = `<div class='graph-legend-compact'><span class='has-popup legend-series'>&#9679; <strong class='legend-series-color' style='--legend-color: ${color};'>${label1}</strong>${popup(v1, m1, u1)}</span>`;
		if (static2 === null) legend += `<span class='legend-divider'></span><span class='has-popup legend-series'>&#9679; <strong class='text-accent'>${label2}</strong>${popup(v2, m2, u2)}</span>`;
		return svg + legend + '</div>';
	}

	function drawSatAreaGraphSVG(d) {
		if (!d.length) return msg('No valid data points found.');
		const count = d.length;
		const used = d.map((p) => p.val1), seen = d.map((p) => p.val2);
		const maxVal = Math.max(Math.max(...seen), Math.max(...used), 1);
		const dataMin = Math.min(Math.min(...used), Math.min(...seen));
		const minVal = Math.max(0, Math.floor((dataMin - 2) / 5) * 5);
		const range = (maxVal - minVal) || 1;
		const xStep = count > 1 ? (W - 2 * PX) / (count - 1) : 0;
		const barW = count > 1 ? xStep : W - 2 * PX;
		const yOf = (v) => (H - PY) - ((v - minVal) / range * (H - 2 * PY));
		const xs = d.map((_, i) => PX + i * xStep), yU = used.map(yOf), yS = seen.map(yOf);
		const bottom = H - PY;

		let hover = '';
		d.forEach((p, i) => {
			hover += `<rect x='${xs[i] - barW / 2}' y='${PY}' width='${barW}' height='${H - 2 * PY}' fill='transparent' class='graph-hit-zone' data-time='${p.time} UTC' data-l1='Active' data-v1='${p.val1}' data-c1='var(--purple)' data-l2='Seen' data-v2='${p.val2}' data-c2='var(--accent)' />`;
		});
		let pathUsed = `M${xs[0]},${bottom}`;
		for (let i = 0; i < count; i++) pathUsed += ` L${xs[i]},${yU[i]}`;
		pathUsed += ` L${xs[count - 1]},${bottom} Z`;
		let pathOver = `M${xs[0]},${yU[0]}`;
		for (let i = 1; i < count; i++) pathOver += ` L${xs[i]},${yU[i]}`;
		for (let i = count - 1; i >= 0; i--) pathOver += ` L${xs[i]},${yS[i]}`;
		pathOver += ' Z';

		let svg = `<svg viewBox='0 0 ${W} ${H}' preserveAspectRatio='xMidYMid meet' class='graph-svg'>`;
		for (let i = 0; i <= 5; i++) {
			const hy = PY + i * (H - 2 * PY) / 5;
			if (i < 5) svg += `<line x1='${PX}' y1='${hy}' x2='${W - PX}' y2='${hy}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
			svg += `<text x='${PX - 10}' y='${hy + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${R(maxVal - i * range / 5)}</text>`;
		}
		for (let i = 0; i <= 5; i++) {
			const idx = Math.max(0, Math.min(Math.trunc((count - 1) * i / 5), count - 1));
			const anchor = i === 0 ? 'start' : (i === 5 ? 'end' : 'middle');
			svg += `<text x='${PX + i * (W - 2 * PX) / 5}' y='${H - PY + 25}' fill='var(--text)' font-family='var(--font-mono)' font-size='12' text-anchor='${anchor}'>${d[idx].time} UTC</text>`;
		}
		svg += `<rect x='${PX}' y='${PY}' width='${W - 2 * PX}' height='${H - 2 * PY}' fill='none' stroke='var(--border)' stroke-width='2' />`;
		svg += `<text x='20' y='${H / 2}' text-anchor='middle' fill='var(--text)' opacity='0.5' font-family='var(--font-sans)' font-size='13' transform='rotate(-90, 20, ${H / 2})'>Satellite Count</text>`;
		svg += `<path d='${pathOver}' fill='var(--accent)' fill-opacity='0.18' stroke='none' />`;
		svg += `<path d='${pathUsed}' fill='var(--purple)' fill-opacity='0.35' stroke='none' />`;
		svg += `<polyline points='${xs.map((x, i) => `${x},${yS[i]}`).join(' ')}' fill='none' stroke='var(--accent)' stroke-width='1.5' stroke-linejoin='round' />`;
		svg += `<polyline points='${xs.map((x, i) => `${x},${yU[i]}`).join(' ')}' fill='none' stroke='var(--purple)' stroke-width='2' stroke-linejoin='round' />`;
		svg += `<g class='hover-layer'>${hover}</g></svg>`;
		const stat = (a) => `<div class='popup legend-stats-popup'><span class='op-muted'>avg</span> <span class='font-mono'>${R(sum(a) / a.length, 1)}</span><br><span class='op-muted'>range</span> <span class='font-mono'>${Math.min(...a)}-${Math.max(...a)}</span></div>`;
		return `${svg}<div class='graph-legend-compact'><span class='has-popup legend-series'>&#9679; <strong class='text-purple'>Sats Active</strong>${stat(used)}</span><span class='legend-divider'></span><span class='has-popup legend-series'>&#9679;<strong class='text-accent'>Sats Seen</strong>${stat(seen)}</span></div>`;
	}

	// The NMEA feed has no TDOP (gpsd derives it), so a series with no data is left out.
	function drawDopSVG(d) {
		if (!d.length) return msg('No DOP history in the selected time window.');
		const all = [
			{ key: 'tdop', label: 'TDOP', color: 'var(--cyan)' }, { key: 'hdop', label: 'HDOP', color: 'var(--green)' },
			{ key: 'vdop', label: 'VDOP', color: 'var(--purple)' }, { key: 'pdop', label: 'PDOP', color: 'var(--amber)' }
		];
		const series = all.filter((s) => d.some((p) => p[s.key] > 0));
		const count = d.length, iW = W - 2 * PX, iH = H - 2 * PY;
		const xStep = count > 1 ? iW / (count - 1) : 0;
		const maxVal = Math.max(...d.flatMap((p) => series.map((s) => p[s.key]))) || 1;

		let svg = `<svg viewBox='0 0 ${W} ${H}' preserveAspectRatio='xMidYMid meet' class='graph-svg'>`;
		for (let i = 0; i <= 5; i++) {
			const vx = PX + i * iW / 5;
			if (i > 0 && i < 5) {
				const hy = PY + i * iH / 5;
				svg += `<line x1='${vx}' y1='${PY}' x2='${vx}' y2='${H - PY}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				svg += `<line x1='${PX}' y1='${hy}' x2='${W - PX}' y2='${hy}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				svg += `<text x='${PX - 10}' y='${hy + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${R(maxVal * (1 - i / 5), 1)}</text>`;
			}
			const idx = Math.max(0, Math.min(Math.round((count - 1) * i / 5), count - 1));
			const anchor = i === 0 ? 'start' : (i === 5 ? 'end' : 'middle');
			svg += `<text x='${PX + idx * xStep}' y='${H - PY + 25}' fill='var(--text)' font-family='var(--font-mono)' font-size='12' text-anchor='${anchor}'>${d[idx].time} UTC</text>`;
		}
		svg += `<text x='${PX - 10}' y='${PY + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${R(maxVal, 1)}</text>`;
		svg += `<text x='${PX - 10}' y='${H - PY + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>0</text>`;
		svg += `<text x='20' y='${H / 2}' text-anchor='middle' fill='var(--text)' opacity='0.5' font-family='var(--font-sans)' font-size='14' font-weight='bold' transform='rotate(-90, 20, ${H / 2})'>DOP</text>`;

		const line = (s) => d.map((p, i) => `${PX + i * xStep},${(H - PY) - (p[s.key] / maxVal * iH)}`);
		series.forEach((s) => {
			const pts = line(s), lastX = PX + (count - 1) * xStep;
			svg += `<path d='M${PX},${H - PY} L${pts.join(' L')} L${lastX},${H - PY} Z' fill='${s.color}' fill-opacity='0.04' stroke='none' />`;
		});
		series.forEach((s) => {
			svg += `<polyline points='${line(s).join(' ')}' fill='none' stroke='${s.color}' stroke-width='2' stroke-linejoin='round' />`;
		});
		const barW = count > 1 ? xStep : iW;
		svg += "<g class='hover-layer'>";
		d.forEach((p, i) => {
			let attrs = ` data-time='${hms(p.ts)} UTC'`;
			series.forEach((s, n) => { attrs += ` data-l${n + 1}='${s.label}' data-v${n + 1}='${p[s.key]}' data-c${n + 1}='${s.color}'`; });
			svg += `<rect x='${PX + i * xStep - barW / 2}' y='${PY}' width='${barW}' height='${iH}' fill='transparent' class='graph-hit-zone'${attrs} />`;
		});
		svg += `</g><rect x='${PX}' y='${PY}' width='${iW}' height='${iH}' fill='none' stroke='var(--border)' stroke-width='1' /></svg>`;

		let legend = "<div class='graph-legend-compact'>";
		series.forEach((s, si) => {
			const vals = d.map((p) => p[s.key]);
			if (si > 0) legend += "<span class='legend-divider'></span>";
			legend += `<span class='has-popup legend-series'>&#9679; <strong class='legend-series-color' style='--legend-color:${s.color}'>${s.label}</strong><div class='popup legend-stats-popup'><span class='op-muted'>avg</span> <span class='font-mono'>${R(sum(vals) / vals.length, 2)}</span><br><span class='op-muted'>range</span> <span class='font-mono'>${R(Math.min(...vals), 2)}-${R(Math.max(...vals), 2)}</span></div></span>`;
		});
		return svg + legend + '</div>';
	}

	function drawSnrConstellationSVG(rows) {
		if (!rows.length) return msg('No SNR history in the selected time window.');
		const meta = {
			G: ['GPS', 'var(--sat-gps)'], R: ['GLONASS', 'var(--sat-glo)'], E: ['Galileo', 'var(--sat-gal)'],
			B: ['BeiDou', 'var(--sat-bds)'], S: ['SBAS', 'var(--sat-sbas)'], Q: ['QZSS', 'var(--sat-qzss)'], I: ['NavIC', 'var(--sat-navic)']
		};
		const d = rows.map((r) => {
			const acc = {};
			r.sats.forEach(([key, snr]) => { if (snr > 0) (acc[key[0]] = acc[key[0]] || []).push(snr); });
			const row = { ts: r.ts, time: hm(r.ts) };
			Object.keys(acc).forEach((p) => { row[p] = Math.round(sum(acc[p]) / acc[p].length * 100) / 100; });
			return row;
		});
		const present = Object.keys(meta).filter((p) => d.some((r) => r[p] !== undefined));
		const count = d.length, iW = W - 2 * PX, iH = H - 2 * PY, snrMax = 48;
		const xStep = count > 1 ? iW / (count - 1) : 0;
		const yOf = (v) => (H - PY) - (v / snrMax * iH);

		let svg = `<svg viewBox='0 0 ${W} ${H}' preserveAspectRatio='xMidYMid meet' class='graph-svg'>`;
		for (let i = 0; i <= 5; i++) {
			if (i > 0 && i < 5) {
				const vx = PX + i * iW / 5, hy = PY + i * iH / 5;
				svg += `<line x1='${vx}' y1='${PY}' x2='${vx}' y2='${H - PY}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				svg += `<line x1='${PX}' y1='${hy}' x2='${W - PX}' y2='${hy}' stroke='var(--border)' stroke-width='1' stroke-dasharray='5,5' />`;
				svg += `<text x='${PX - 10}' y='${hy + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${R(snrMax * (1 - i / 5))}</text>`;
			}
			const idx = Math.max(0, Math.min(Math.round((count - 1) * i / 5), count - 1));
			const anchor = i === 0 ? 'start' : (i === 5 ? 'end' : 'middle');
			svg += `<text x='${PX + idx * xStep}' y='${H - PY + 25}' fill='var(--text)' font-family='var(--font-mono)' font-size='12' text-anchor='${anchor}'>${d[idx].time} UTC</text>`;
		}
		svg += `<text x='${PX - 10}' y='${PY + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>${snrMax}</text>`;
		svg += `<text x='${PX - 10}' y='${H - PY + 4}' fill='var(--text)' opacity='0.5' font-family='var(--font-mono)' font-size='10' text-anchor='end'>0</text>`;
		svg += `<text x='20' y='${H / 2}' text-anchor='middle' fill='var(--text)' opacity='0.5' font-family='var(--font-sans)' font-size='14' font-weight='bold' transform='rotate(-90, 20, ${H / 2})'>SNR (dBHz)</text>`;
		[[38, 'var(--cyan)'], [30, 'var(--green)'], [22, 'var(--yellow)'], [15, 'var(--amber)']].forEach(([s, c]) => {
			const ty = yOf(s);
			svg += `<line x1='${PX}' y1='${ty}' x2='${W - PX}' y2='${ty}' stroke='${c}' stroke-width='1' stroke-dasharray='4,4' opacity='0.25' />`;
		});
		const pointsOf = (p) => d.map((r, i) => (r[p] === undefined ? null : [PX + i * xStep, yOf(r[p])])).filter(Boolean);
		present.forEach((p) => {
			const pts = pointsOf(p);
			if (!pts.length) return;
			svg += `<path d='M${pts[0][0]},${H - PY} L${pts.map((q) => q.join(',')).join(' L')} L${pts[pts.length - 1][0]},${H - PY} Z' fill='${meta[p][1]}' fill-opacity='0.04' stroke='none' />`;
		});
		present.forEach((p) => {
			const pts = pointsOf(p);
			if (pts.length) svg += `<polyline points='${pts.map((q) => q.join(',')).join(' ')}' fill='none' stroke='${meta[p][1]}' stroke-width='2' stroke-linejoin='round' />`;
		});
		const barW = count > 1 ? xStep : iW;
		svg += "<g class='hover-layer'>";
		d.forEach((r, i) => {
			let attrs = ` data-time='${hms(r.ts)} UTC'`;
			present.forEach((p, n) => {
				attrs += ` data-l${n + 1}='${meta[p][0]}' data-v${n + 1}='${r[p] !== undefined ? `${r[p]} dBHz` : '-'}' data-c${n + 1}='${meta[p][1]}'`;
			});
			svg += `<rect x='${PX + i * xStep - barW / 2}' y='${PY}' width='${barW}' height='${iH}' fill='transparent' class='graph-hit-zone'${attrs} />`;
		});
		svg += `</g><rect x='${PX}' y='${PY}' width='${iW}' height='${iH}' fill='none' stroke='var(--border)' stroke-width='1' /></svg>`;
		let legend = "<div class='graph-legend-compact'>";
		present.forEach((p, si) => {
			const vals = d.map((r) => r[p]).filter((v) => v !== undefined);
			if (si > 0) legend += "<span class='legend-divider'></span>";
			legend += `<span class='has-popup legend-series'>&#9679; <strong class='legend-series-color' style='--legend-color:${meta[p][1]}'>${meta[p][0]}</strong><div class='popup legend-stats-popup'><span class='op-muted'>avg</span> <span class='font-mono'>${R(sum(vals) / vals.length, 1)} dBHz</span><br><span class='op-muted'>range</span> <span class='font-mono'>${R(Math.min(...vals), 1)}-${R(Math.max(...vals), 1)} dBHz</span></div></span>`;
		});
		return svg + legend + '</div>';
	}

	// ---- history bundles ----

	function parseBundle(text) {
		const out = { track: [], stats: [], sat: [], snr: [], dop: [], pps: [], snrhist: [] };
		let cur = null;
		text.split('\n').forEach((line) => {
			if (!line) return;
			if (line[0] === '#') { cur = out[line.slice(1)] || null; return; }
			if (!cur) return;
			const p = line.trim().split(' ');
			const ts = parseInt(p[0], 10);
			if (!ts) return;
			if (cur === out.snrhist) {
				cur.push({ ts, sats: p.slice(1).map((x) => { const q = x.split(':'); return [q[0], parseFloat(q[1])]; }) });
			} else {
				cur.push([ts].concat(p.slice(1).map(Number)));
			}
		});
		return out;
	}

	const cache = {};
	async function bundle(name, maxAge) {
		const c = cache[name];
		if (c && Date.now() - c.at < maxAge) return c.parsed;
		const r = await data(`${name}.txt`);
		if (!r.ok) return c ? c.parsed : parseBundle('');
		const parsed = parseBundle(await r.text());
		cache[name] = { at: Date.now(), parsed };
		return parsed;
	}

	function buildGraphs(h, hours) {
		const newest = Math.max(0, ...Object.values(h).map((a) => (a.length ? (a[a.length - 1].ts || a[a.length - 1][0]) : 0)));
		const cutoff = newest - hours * 3600;
		const rows = (a) => a.filter((r) => r[0] >= cutoff);
		const pair = (a, i, j) => rows(a).map((r) => ({ time: hm(r[0]), val1: r[i], val2: j === null ? 0 : r[j] }));
		const none = msg('No history yet. Data accumulates automatically.');
		const g = (a, f) => (rows(a).length ? f() : none);
		return {
			tracking: g(h.track, () => drawGraphSVG(pair(h.track, 3, 1), 'Offset', 'Freq', 's', 'ppm', 'var(--green)')),
			stats: g(h.stats, () => drawGraphSVG(pair(h.stats, 1, 2), 'Est Offset', 'Std Dev', 's', 's', 'var(--indigo)')),
			measures: msg('This module has no network NTP peers, so there are no measurements to plot.'),
			rootdelaydisp: g(h.track, () => drawGraphSVG(pair(h.track, 4, null), 'Root Disp', '', 's', 's', 'var(--red)', false, '')),
			frequency: g(h.track, () => drawGraphSVG(pair(h.track, 1, 2), 'Frequency', 'Skew', 'ppm', 'ppm', 'var(--lime)', true)),
			satcounts: g(h.sat, () => drawSatAreaGraphSVG(pair(h.sat, 2, 1))),
			snr: g(h.snr, () => drawGraphSVG(pair(h.snr, 1, 2), 'SNR Used', 'SNR All', 'dBHz', 'dBHz', 'var(--pink)')),
			dop: g(h.dop, () => drawDopSVG(rows(h.dop).map((r) => ({ ts: r[0], time: hm(r[0]), tdop: r[1], hdop: r[2], vdop: r[3], pdop: r[4] })))),
			snr_constellation: drawSnrConstellationSVG(h.snrhist.filter((r) => r.ts >= cutoff)),
			pps: g(h.pps, () => drawGraphSVG(pair(h.pps, 1, 2), 'PPS Offset', 'RMS Offset', 's', 's', 'var(--cyan)'))
		};
	}

	// about one point a minute over the last hour, as the original
	function sparklines(h) {
		const out = {};
		h.snrhist.forEach((r, i) => {
			if (i % 2 !== 0) return;
			r.sats.forEach(([key, snr]) => { if (snr > 0) (out[key] = out[key] || []).push(Math.round(snr * 10) / 10); });
		});
		return out;
	}

	// ---- request routing ----

	let lastTs = 0, lastChange = Date.now();

	async function main(hours) {
		const r = await data('live.json');
		if (!r.ok) throw new Error('no data');
		const live = await r.json();
		showNode(live.node);
		if (live.ts !== lastTs) { lastTs = live.ts; lastChange = Date.now(); }
		// the collector stopped: show the original's "services offline" overlay
		if (Date.now() - lastChange > 60000) live.service_status = { chrony: false, gpsd: false };
		const name = hours <= 1 ? 'hist1' : (hours <= 6 ? 'hist6' : 'hist24');
		const recent = await bundle('hist1', 25000);
		const long = name === 'hist1' ? recent : await bundle(name, 120000);
		// the longer bundles are rebuilt rarely, so just after a start they can be behind
		const h = long.track.length >= recent.track.length ? long : recent;
		live.graphs = buildGraphs(h, hours);
		live.sat_sparklines = sparklines(recent);
		live.server_version = VERSION;
		return json(live);
	}

	window.fetch = function (url, opts) {
		if (typeof url !== 'string' || !url.startsWith('?ajax=')) return realFetch(url, opts);
		const q = new URLSearchParams(url.slice(1));
		const what = q.get('ajax');
		if (what === '1') return main(parseInt(q.get('hours'), 10) || 1);
		if (what === 'clients') return data('clients.json');
		if (what === 'version') return Promise.resolve(json({ status: 'disabled' }));
		return Promise.resolve(json({ error: 'Not available on this device.' }));
	};
})();
