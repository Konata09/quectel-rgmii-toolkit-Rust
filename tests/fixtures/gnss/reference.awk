# Builds live.json in the shape of ChroGPS Dash's ?ajax=1 reply (without graphs,
# which the browser draws) and appends the history logs.
# Busybox awk: no gensub, every variable is global unless it is a parameter, and
# "name (" parses as a function call, so concatenation needs "name \"\" (".

function trim(s) { gsub(/^[ \t\r]+|[ \t\r]+$/, "", s); return s }

function js(s) {
	gsub(/\\/, "\\\\", s)
	gsub(/"/, "\\\"", s)
	gsub(/\t/, " ", s)
	return "\"" s "\""
}

function kv(k, v) { return js(k) ":" js(v) }

# "1.5us" style chronyc values to seconds
function to_sec(s, v) {
	v = s + 0
	if (s ~ /ns$/) return v * 1e-9
	if (s ~ /us$/) return v * 1e-6
	if (s ~ /ms$/) return v * 1e-3
	return v
}

function num_or_na(v) { return v == "" ? "\"N/A\"" : (v + 0) }

function ddmm(v, d) { d = int(v / 100); return d + (v - d * 100) / 60 }

# key:value blocks from chronyc; fills keys[] in order, returns the JSON members
function read_kv(file, vals, cnt, line, i, k, out) {
	out = ""
	cnt = 0
	while ((getline line < file) > 0) {
		i = index(line, ":")
		if (!i) continue
		k = trim(substr(line, 1, i - 1))
		vals[k] = trim(substr(line, i + 1))
		out = out "" (cnt++ ? "," : "") kv(k, vals[k])
	}
	close(file)
	return out
}

function read_sources(file, line, f, n, j, out, off) {
	out = ""
	while ((getline line < file) > 0) {
		if (line !~ /^[\^=#]/) continue
		n = split(line, f, /[ \t]+/)
		if (n < 7) continue
		off = f[7]
		for (j = 8; j <= n; j++) off = off " " f[j]
		out = out "" (out == "" ? "" : ",") "{" kv("state", f[1]) "," kv("name", f[2]) "," kv("stratum", f[3]) "," \
			kv("poll", f[4]) "," kv("reach", f[5]) "," kv("lastrx", f[6]) "," kv("offset", off) "}"
	}
	close(file)
	return out
}

function read_conf(file, line, f) {
	max_drift = 500000
	drift_default = 1
	while ((getline line < file) > 0) {
		if (line ~ /^[ \t]*(#|$)/) continue
		split(trim(line), f, /[ \t]+/)
		if (f[1] == "maxdrift") { max_drift = f[2] + 0; drift_default = 0 }
		if (f[1] == "makestep") { step_thr = f[2]; step_lim = f[3] + 0; have_step = 1 }
		if (f[1] == "refclock") {
			if (match(line, /poll +-?[0-9]+/)) { split(substr(line, RSTART, RLENGTH), a, / +/); ref_poll = a[2]; have_poll = 1 }
			if (match(line, /precision +[0-9.e+-]+/)) { split(substr(line, RSTART, RLENGTH), a, / +/); ref_prec = a[2]; have_prec = 1 }
		}
	}
	close(file)
}

function uptime_str(t, d, h, m, s) {
	d = int(t / 86400); h = int(t % 86400 / 3600); m = int(t % 3600 / 60)
	s = ""
	if (d > 0) s = d "d "
	if (h > 0) s = s h "h "
	return s m "m"
}

# ---- NMEA ----

function add_sat(sys, id, el, az, ss, sig, prn, gid, pre, key) {
	if (id == "" || id + 0 == 0) return
	id += 0
	if (sys == "GP") {
		if (id >= 33 && id <= 64) { prn = id + 87; gid = 1; pre = "S" } else { prn = id; gid = 0; pre = "G" }
	} else if (sys == "GL") { prn = id; gid = 6; pre = "R" }
	else if (sys == "GA") { prn = 300 + id; gid = 2; pre = "E" }
	else if (sys == "GB") { prn = 400 + id; gid = 3; pre = "B" }
	else if (sys == "GQ") { prn = 192 + id; gid = 5; pre = "Q" }
	else if (sys == "GI") { prn = 800 + id; gid = 7; pre = "I" }
	else return
	key = prn "_" gid "_" sig
	if (!(key in s_prn)) s_order[++s_cnt] = key
	s_prn[key] = prn; s_gid[key] = gid; s_svid[key] = id; s_sig[key] = sig
	s_el[key] = el + 0; s_az[key] = az + 0; s_ss[key] = ss + 0
	s_pre[key] = pre
	s_usekey[key] = sysnum[sys] "," id
}

# NMEA signal IDs to the numbering the front end's band labels expect (gpsd's)
function sig_map(sys, id) {
	if (sys == "GP") { if (id == 1) return 0; if (id == 7) return 6; if (id == 8) return 7 }
	if (sys == "GL") { if (id == 1) return 0; if (id == 3) return 2 }
	if (sys == "GA") { if (id == 7) return 2; if (id == 1) return 5; if (id == 2) return 8 }
	if (sys == "GB") { if (id == 1) return 2; if (id == 3) return 3; if (id == 5) return 5; if (id == 6) return 6 }
	if (sys == "GQ") { if (id == 1) return 0; if (id == 8) return 8 }
	return id
}

function read_nmea(file, line, f, n, i, sys, sig, t) {
	sysnum["GP"] = 1; sysnum["GL"] = 2; sysnum["GA"] = 3; sysnum["GB"] = 4; sysnum["GQ"] = 5; sysnum["GI"] = 6
	s_cnt = 0
	while ((getline line < file) > 0) {
		i = index(line, "$")
		if (!i) continue
		line = substr(line, i + 1)
		sub(/\*.*$/, "", line)
		n = split(line, f, ",")
		t = substr(f[1], 3)
		sys = substr(f[1], 1, 2)
		if (t == "GGA" && gga_q == "") {
			if (f[3] != "" && f[5] != "") {
				lat = ddmm(f[3] + 0); if (f[4] == "S") lat = -lat
				lon = ddmm(f[5] + 0); if (f[6] == "W") lon = -lon
				alt = f[10]
			}
			gga_q = f[7]
		} else if (t == "GSA") {
			# fields: 2 mode 3 fix, 4-15 ids, 16 pdop 17 hdop 18 vdop 19 system
			if (fix_mode == "") { fix_mode = f[3]; pdop = f[16]; hdop = f[17]; vdop = f[18] }
			if (n >= 19) for (i = 4; i <= 15; i++) if (f[i] != "") used[(f[19] + 0) "," (f[i] + 0)] = 1
		} else if (t == "GSV") {
			sig = sig_map(sys, ((n - 4) % 4 == 1) ? f[n] + 0 : 1)
			for (i = 5; i + 3 <= n; i += 4) add_sat(sys, f[i], f[i + 1], f[i + 2], f[i + 3], sig)
		}
	}
	close(file)
}

BEGIN {
	# tracking
	trk_json = read_kv(dir "/.tracking", trk)
	srv_json = read_kv(dir "/.serverstats", srv)
	sources_json = read_sources(dir "/.sources")
	read_conf(conf)

	chrony_ok = trk_json != ""

	# freq limit
	split(trk["Frequency"], ff, " ")
	freq_abs = ff[1] + 0
	freq_signed = ff[2] == "ppm" && ff[3] == "slow" ? -freq_abs : freq_abs
	prox = int(freq_abs / max_drift * 1000 + 0.5) / 10
	if (prox > 100) prox = 100
	pxc = prox < 50 ? "text-green" : (prox < 80 ? "text-amber" : "text-red")
	drift_lbl = max_drift " ppm" (drift_default ? " (default)" : "")

	# clock steps
	steps = 0
	last_step = "Never"
	while ((getline line < (dir "/.steps")) > 0) {
		steps++
		sline = line
	}
	close(dir "/.steps")
	if (steps) {
		if (match(sline, /stepped by [-+0-9.e]+/)) mag = substr(sline, RSTART + 11, RLENGTH - 11)
		last_step = substr(sline, 1, 15) " UTC · " mag " s"
	}
	sc = steps == 0 ? "text-green" : (steps <= 2 ? "text-amber" : "text-red")

	sync_json = trk_json
	if (chrony_ok) {
		sync_json = sync_json "," kv("Clock Steps (24h)", "<span class='" sc " font-bold'>" steps "</span>")
		sync_json = sync_json "," kv("Last Step", last_step)
		sync_json = sync_json "," kv("Freq Limit", "<span class='" pxc " font-bold'>" prox "%</span> of " drift_lbl)
		if (chrony_up + 0 > 0)
			sync_json = sync_json "," kv("Chrony Uptime", "<div class=\"has-popup\"><span class=\"tip-text tip-text-muted\">" \
				uptime_str(chrony_up + 0) "</span><div class=\"popup\">Chrony running since:<br><strong>" \
				strftime("%Y-%m-%d %H:%M", now - chrony_up, 1) " UTC</strong></div></div>")
	}

	# NTP configuration
	cfg_json = ""
	if (have_step) cfg_json = kv("Make Step", step_thr "s - " (step_lim == -1 ? "always" : "first " step_lim " update" (step_lim != 1 ? "s" : "")))
	cfg_json = cfg_json "" (cfg_json == "" ? "" : ",") kv("Max Drift", max_drift " ppm" (drift_default ? " <span class='platform-meta'>(default)</span>" : ""))
	if (have_poll) cfg_json = cfg_json "," kv("Refclock Poll", (2 ^ ref_poll) "s <span class=\"platform-meta\">(2^" ref_poll ")</span>")
	if (have_prec) cfg_json = cfg_json "," kv("Ref Precision", sprintf("%.6f", ref_prec) "s")

	# GNSS
	read_nmea(dir "/.nmea")

	sats_json = ""
	seen = 0; used_n = 0; n_all = 0; sum_all = 0; n_used = 0; sum_used = 0
	hist_line = ""
	# GSA only names satellites, but a satellite can have a row per signal and the
	# score averages every row marked used. Mark only the row the front end shows:
	# the strongest, or the higher signal ID on a tie.
	for (i = 1; i <= s_cnt; i++) {
		k = s_order[i]
		u = s_usekey[k]
		if (!(u in used)) continue
		if (!(u in use_row) || s_ss[k] > s_ss[use_row[u]] || (s_ss[k] == s_ss[use_row[u]] && s_sig[k] > s_sig[use_row[u]]))
			use_row[u] = k
	}
	for (i = 1; i <= s_cnt; i++) {
		k = s_order[i]
		u = s_usekey[k]
		is_used = ((u in use_row) && use_row[u] == k) ? 1 : 0
		s_used[k] = is_used
		sats_json = sats_json "" (i > 1 ? "," : "") js(k) ":{\"PRN\":" s_prn[k] ",\"gnssid\":" s_gid[k] ",\"svid\":" s_svid[k] \
			",\"sigid\":" s_sig[k] ",\"el\":" s_el[k] ",\"az\":" s_az[k] ",\"ss\":" s_ss[k] ",\"used\":" (is_used ? "true" : "false") \
			",\"health\":" (is_used ? 1 : 0) ",\"freqid\":null,\"last_seen\":" now "}"
		if (s_ss[k] > 0) {
			n_all++; sum_all += s_ss[k]
			if (is_used) { n_used++; sum_used += s_ss[k] }
		}
		# one figure per satellite: the strongest signal, as the original counts them
		p = s_prn[k]
		if (!(p in best) || s_ss[k] > best[p]) { best[p] = s_ss[k]; best_pre[p] = s_pre[k] }
		if (is_used) p_used[p] = 1
		if (!(p in p_ghost)) p_ghost[p] = 1
		if (s_el[k] != 0 || s_az[k] != 0 || s_ss[k] != 0 || is_used) p_ghost[p] = 0
	}
	for (p in best) {
		if (!p_ghost[p]) { seen++; if (p in p_used) used_n++ }
		if (best[p] > 0) hist_line = hist_line " " best_pre[p] p ":" sprintf("%.2f", best[p])
	}

	fix = fix_mode == "3" ? "3D Fix" : (fix_mode == "2" ? "2D Fix" : "No Fix")
	st = "Unknown"
	if (gga_q == "1") st = "Normal"
	else if (gga_q == "2") st = "DGPS"
	else if (gga_q == "4") st = "RTK Fixed"
	else if (gga_q == "5") st = "RTK Float"
	else if (gga_q == "6") st = "DR"

	lat_j = lat == "" ? "\"N/A\"" : sprintf("%.6f", lat)
	lon_j = lon == "" ? "\"N/A\"" : sprintf("%.6f", lon)
	alt_j = alt == "" ? "\"N/A\"" : sprintf("%.1f", alt)

	dev_ext = "Hardware: &nbsp;Quectel RM520N-GL\\nFirmware: &nbsp;" fw "\\nGPS Active Since: &nbsp;" started

	printf "{\"ts\":%d,\"sources\":[%s],", now, sources_json
	printf "\"tracking\":{\"Sync Status\":{%s},\"Server Statistics\":{%s},\"NTP Configuration\":{%s}},", sync_json, srv_json, cfg_json
	printf "\"gps\":{\"lat\":%s,\"lon\":%s,\"alt\":%s,\"sats\":{%s},\"fix\":%s,\"status\":%s,", lat_j, lon_j, alt_j, sats_json, js(fix), js(st)
	printf "\"tdop\":\"N/A\",\"hdop\":%s,\"vdop\":%s,\"pdop\":%s,\"ept\":\"N/A\",", num_or_na(hdop), num_or_na(vdop), num_or_na(pdop)
	printf "\"device\":\"RM520N-GL @ AT+QGPSGNMEA\",\"device_extended\":\"%s\",", dev_ext
	printf "\"gpsd_version\":\"\",\"gpsd_revision\":\"\",\"gpsd_protocol\":\"\",\"activated\":%s},", js(started)
	printf "\"service_status\":{\"chrony\":%s,\"gpsd\":%s}}\n", (chrony_ok ? "true" : "false"), (at_ok + 0 ? "true" : "false")

	if (append + 0) {
		if (chrony_ok) {
			split(trk["Last offset"], o, " ")
			split(trk["RMS offset"], r, " ")
			split(trk["Skew"], sk, " ")
			split(trk["Root dispersion"], rd, " ")
			split(trk["System time"], sy, " ")
			sys_off = sy[3] == "slow" ? -sy[1] : sy[1] + 0
			printf "%d %s %s %s %s\n", now, freq_signed, sk[1] + 0, o[1] + 0, rd[1] + 0 >> (dir "/track.log")
			close(dir "/track.log")
			printf "%d %.9f %s\n", now, sys_off, r[1] >> (dir "/pps.log")
			close(dir "/pps.log")
		}
		# first source line that has statistics
		while ((getline line < (dir "/.sourcestats")) > 0) {
			n = split(trim(line), f, /[ \t]+/)
			if (n >= 8 && f[1] != "Name/IP" && f[1] !~ /^=/ && !stat_done) {
				printf "%d %.12f %.12f\n", now, to_sec(f[7]), to_sec(f[8]) >> (dir "/stats.log")
				close(dir "/stats.log")
				stat_done = 1
			}
		}
		close(dir "/.sourcestats")
		if (at_ok + 0 && s_cnt > 0) {
			printf "%d %d %d\n", now, seen, used_n >> (dir "/sat.log")
			close(dir "/sat.log")
			printf "%d %.2f %.2f\n", now, (n_used ? sum_used / n_used : 0), (n_all ? sum_all / n_all : 0) >> (dir "/snr.log")
			close(dir "/snr.log")
			if (hist_line != "") {
				printf "%d%s\n", now, hist_line >> (dir "/snrhist.log")
				close(dir "/snrhist.log")
			}
			if (pdop != "") {
				printf "%d 0 %s %s %s\n", now, hdop, vdop, pdop >> (dir "/dop.log")
				close(dir "/dop.log")
			}
		}
	}
}
