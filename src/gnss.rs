//! GNSS timing page: chrony and NMEA state for the ChroGPS Dash front end in `www/gnss/`.
//!
//! A port of gnss-ppsd's `gnss-dash-poll` and `gnss-dash.awk`. Every 10 s it reads NMEA
//! through the shared AT queue and runs `chronyc`; every 30 s, once chrony has synchronised,
//! it adds a history sample. History stays in memory (24 h) and is lost on restart, so
//! nothing is written to flash. Nothing runs unless chronyc is installed.
use crate::at::At;
use regex::Regex;
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

const PERIOD: Duration = Duration::from_secs(10);
/// Cycles between history samples (30 s, as the original) and between slow tasks (5 min).
const HIST_EVERY: u64 = 3;
const LONG_EVERY: u64 = 30;
/// 24 h of 30 s samples.
const MAX_SAMPLES: usize = 2880;
/// Per-satellite SNR is kept for the last hour (the 1 h bundle and the sparklines); the
/// longer graphs only need each constellation's average.
const DETAIL_SAMPLES: usize = 120;
pub const NMEA: [&str; 3] = [
    "AT+QGPSGNMEA=\"GGA\"",
    "AT+QGPSGNMEA=\"GSA\"",
    "AT+QGPSGNMEA=\"GSV\"",
];
const MESSAGES: &str = "/var/log/messages";
/// Constellation letters in the order the front end lists them.
const CONSTELLATIONS: [u8; 7] = *b"GRBESQI";

// ---- awk value semantics ----

/// awk's string-to-number conversion: the longest numeric prefix, 0 when there is none.
fn lead(s: &str) -> f64 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\r']);
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1
        }
        *i > start
    };
    let mut any = digits(&mut i);
    if i < b.len() && b[i] == b'.' {
        i += 1;
        any |= digits(&mut i);
    }
    if !any {
        return 0.0;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1
        }
        if digits(&mut j) {
            i = j
        }
    }
    s[..i].parse().unwrap_or(0.0)
}
fn trim_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}
/// awk's number-to-string conversion: integers as integers, the rest as `%.6g`.
fn awk_number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    let e = format!("{x:.5e}");
    let (mantissa, exponent) = e.split_once('e').unwrap_or((&e, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_zeros(mantissa), exponent.abs())
    } else {
        trim_zeros(&format!("{x:.*}", (5 - exponent) as usize)).to_owned()
    }
}
/// A JSON number the way awk writes `x + 0`: integral values without a fraction.
fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}
/// A JSON number written with `%.Nf`.
fn fixed(x: f64, decimals: usize) -> Value {
    json!(format!("{x:.decimals$}").parse::<f64>().unwrap_or(0.0))
}

/// JSON object that keeps insertion order (serde_json's map sorts its keys).
struct Ordered<V>(Vec<(String, V)>);
impl<V: Serialize> Serialize for Ordered<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

// ---- chronyc output ----

/// `key : value` lines of `chronyc tracking` and `serverstats`, in order.
fn key_values(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}
fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}
fn sources(text: &str) -> Vec<Ordered<String>> {
    text.lines()
        .filter(|line| line.starts_with(['^', '=', '#']))
        .filter_map(|line| {
            let f: Vec<&str> = line.split([' ', '\t']).filter(|s| !s.is_empty()).collect();
            if f.len() < 7 {
                return None;
            }
            let names = ["state", "name", "stratum", "poll", "reach", "lastrx"];
            let mut row: Vec<(String, String)> = names
                .iter()
                .zip(&f)
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            row.push(("offset".into(), f[6..].join(" ")));
            Some(Ordered(row))
        })
        .collect()
}
/// `"1.5us"` style chronyc values in seconds.
fn seconds(s: &str) -> f64 {
    let v = lead(s);
    if s.ends_with("ns") {
        v * 1e-9
    } else if s.ends_with("us") {
        v * 1e-6
    } else if s.ends_with("ms") {
        v * 1e-3
    } else {
        v
    }
}
/// Estimated offset and standard deviation of the first source with statistics.
fn source_stats(text: &str) -> Option<[f64; 2]> {
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        (f.len() >= 8 && f[0] != "Name/IP" && !f[0].starts_with('='))
            .then(|| [seconds(f[6]), seconds(f[7])])
    })
}
/// `chronyc -n clients` in the shape of ChroGPS Dash's `?ajax=clients` reply.
pub fn clients(text: &str) -> String {
    let clients: Vec<Value> = text
        .lines()
        .filter(|line| !line.starts_with("Hostname") && !line.starts_with("===="))
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 || f[5] == "-" {
                return None;
            }
            let last = f[5];
            let unit = match last.as_bytes().last() {
                Some(b'm') => 60.0,
                Some(b'h') => 3600.0,
                Some(b'd') => 86400.0,
                Some(b'y') => 31536000.0,
                _ => 1.0,
            };
            Some(
                json!({"ip":f[0],"ntp_hits":f[1],"ntp_drops":f[2],"last_ntp":last,
                "last_ntp_seconds":(lead(last) * unit) as i64}),
            )
        })
        .collect();
    json!({ "clients": clients }).to_string()
}
#[derive(Default)]
struct Conf {
    max_drift: f64,
    drift_default: bool,
    step: Option<(String, f64)>,
    poll: Option<String>,
    precision: Option<String>,
}
fn conf(text: &str) -> Conf {
    let mut c = Conf {
        max_drift: 500000.0,
        drift_default: true,
        ..Default::default()
    };
    static POLL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"poll +(-?[0-9]+)").unwrap());
    static PRECISION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"precision +([0-9.e+-]+)").unwrap());
    let capture = |re: &Regex, line: &str| Some(re.captures(line)?[1].to_owned());
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = trimmed
            .split([' ', '\t'])
            .filter(|s| !s.is_empty())
            .collect();
        match f[0] {
            "maxdrift" => {
                c.max_drift = lead(f.get(1).unwrap_or(&""));
                c.drift_default = false
            }
            "makestep" => {
                c.step = Some((
                    f.get(1).unwrap_or(&"").to_string(),
                    lead(f.get(2).unwrap_or(&"")),
                ))
            }
            "refclock" => {
                if let Some(poll) = capture(&POLL, line) {
                    c.poll = Some(poll)
                }
                if let Some(precision) = capture(&PRECISION, line) {
                    c.precision = Some(precision)
                }
            }
            _ => {}
        }
    }
    c
}

// ---- NMEA ----

struct SatRow {
    prn: u32,
    gid: u8,
    svid: u32,
    sig: u32,
    el: f64,
    az: f64,
    ss: f64,
    prefix: u8,
    /// System and satellite ID as GSA names it.
    used_key: (u32, u32),
}
#[derive(Default)]
struct Nmea {
    lat: Option<f64>,
    lon: Option<f64>,
    alt: String,
    quality: String,
    fix_mode: String,
    pdop: String,
    hdop: String,
    vdop: String,
    used: std::collections::HashSet<(u32, u32)>,
    sats: Vec<SatRow>,
    index: HashMap<(u32, u8, u32), usize>,
}
fn ddmm(v: f64) -> f64 {
    let d = (v / 100.0).trunc();
    d + (v - d * 100.0) / 60.0
}
/// NMEA signal IDs to the numbering the front end's band labels expect (gpsd's).
fn signal(sys: &str, id: u32) -> u32 {
    match (sys, id) {
        ("GP", 1) | ("GL", 1) | ("GQ", 1) => 0,
        ("GP", 7) => 6,
        ("GP", 8) => 7,
        ("GL", 3) | ("GA", 7) | ("GB", 1) => 2,
        ("GA", 1) => 5,
        ("GA", 2) => 8,
        ("GQ", 8) => 8,
        _ => id,
    }
}
impl Nmea {
    fn add_sat(&mut self, sys: &str, f: &[&str], sig: u32) {
        let id = lead(f[0]);
        if id == 0.0 {
            return;
        }
        let id = id as u32;
        let (prn, gid, prefix, system) = match sys {
            "GP" if (33..=64).contains(&id) => (id + 87, 1, b'S', 1),
            "GP" => (id, 0, b'G', 1),
            "GL" => (id, 6, b'R', 2),
            "GA" => (300 + id, 2, b'E', 3),
            "GB" => (400 + id, 3, b'B', 4),
            "GQ" => (192 + id, 5, b'Q', 5),
            "GI" => (800 + id, 7, b'I', 6),
            _ => return,
        };
        let row = SatRow {
            prn,
            gid,
            svid: id,
            sig,
            el: lead(f[1]),
            az: lead(f[2]),
            ss: lead(f[3]),
            prefix,
            used_key: (system, id),
        };
        match self.index.get(&(prn, gid, sig)) {
            Some(&i) => self.sats[i] = row,
            None => {
                self.index.insert((prn, gid, sig), self.sats.len());
                self.sats.push(row)
            }
        }
    }
    fn parse(text: &str) -> Self {
        let mut n = Self::default();
        for line in text.lines() {
            let Some(at) = line.find('$') else { continue };
            let body = &line[at + 1..];
            let body = body.split_once('*').map_or(body, |(b, _)| b);
            let f: Vec<&str> = body.split(',').collect();
            let field = |i: usize| f.get(i - 1).copied().unwrap_or("");
            let head = f[0];
            if head.len() < 2 || !head.is_char_boundary(2) {
                continue;
            }
            let (sys, kind) = head.split_at(2);
            match kind {
                "GGA" if n.quality.is_empty() => {
                    if !field(3).is_empty() && !field(5).is_empty() {
                        let lat = ddmm(lead(field(3)));
                        let lon = ddmm(lead(field(5)));
                        n.lat = Some(if field(4) == "S" { -lat } else { lat });
                        n.lon = Some(if field(6) == "W" { -lon } else { lon });
                        n.alt = field(10).into();
                    }
                    n.quality = field(7).into();
                }
                "GSA" => {
                    if n.fix_mode.is_empty() {
                        n.fix_mode = field(3).into();
                        n.pdop = field(16).into();
                        n.hdop = field(17).into();
                        n.vdop = field(18).into();
                    }
                    if f.len() >= 19 {
                        let system = lead(field(19)) as u32;
                        for i in 4..=15 {
                            if !field(i).is_empty() {
                                n.used.insert((system, lead(field(i)) as u32));
                            }
                        }
                    }
                }
                "GSV" => {
                    let count = f.len();
                    let id = if count >= 4 && (count - 4) % 4 == 1 {
                        lead(f[count - 1]) as u32
                    } else {
                        1
                    };
                    let sig = signal(sys, id);
                    let mut i = 4;
                    while i + 4 <= count {
                        n.add_sat(sys, &f[i..i + 4], sig);
                        i += 4;
                    }
                }
                _ => {}
            }
        }
        n
    }
}

// ---- one collection ----

/// What one collection cycle read.
pub struct Inputs<'a> {
    pub now: i64,
    pub tracking: &'a str,
    pub serverstats: &'a str,
    pub sources: &'a str,
    pub sourcestats: &'a str,
    pub conf: &'a str,
    pub nmea: &'a str,
    /// Every NMEA query got an answer (an ERROR reply counts: GNSS may just be off).
    pub at_ok: bool,
    /// Clock steps chronyd logged, and the newest such line.
    pub steps: (usize, &'a str),
    /// Seconds chronyd has been running, 0 when unknown.
    pub chrony_up: i64,
    /// First synchronisation, ISO 8601 UTC, empty before it.
    pub started: &'a str,
    pub model: &'a str,
    pub firmware: &'a str,
}
/// The history values one cycle produced; each series is present only when it could be read.
#[derive(Default, Clone)]
pub struct Record {
    track: Option<[f64; 4]>,
    pps: Option<[f64; 2]>,
    stats: Option<[f64; 2]>,
    sat: Option<[u16; 2]>,
    snr: Option<[f64; 2]>,
    dop: Option<[f64; 3]>,
    /// Strongest SNR of each satellite: constellation letter, PRN, dBHz.
    snrhist: Vec<(u8, u32, f64)>,
}
#[derive(Serialize)]
struct Sat {
    #[serde(rename = "PRN")]
    prn: u32,
    gnssid: u8,
    svid: u32,
    sigid: u32,
    el: Value,
    az: Value,
    ss: Value,
    used: bool,
    health: u8,
    freqid: Option<u8>,
    last_seen: i64,
}
#[derive(Serialize)]
struct Tracking {
    #[serde(rename = "Sync Status")]
    sync: Ordered<String>,
    #[serde(rename = "Server Statistics")]
    server: Ordered<String>,
    #[serde(rename = "NTP Configuration")]
    config: Ordered<String>,
}
#[derive(Serialize)]
struct Gps {
    lat: Value,
    lon: Value,
    alt: Value,
    sats: Ordered<Sat>,
    fix: &'static str,
    status: &'static str,
    tdop: &'static str,
    hdop: Value,
    vdop: Value,
    pdop: Value,
    ept: &'static str,
    device: String,
    device_extended: String,
    gpsd_version: &'static str,
    gpsd_revision: &'static str,
    gpsd_protocol: &'static str,
    activated: String,
}
#[derive(Serialize)]
struct Services {
    chrony: bool,
    gpsd: bool,
}
#[derive(Serialize)]
struct Live {
    ts: i64,
    sources: Vec<Ordered<String>>,
    tracking: Tracking,
    gps: Gps,
    service_status: Services,
    /// Module model, for the page header.
    node: String,
}
fn uptime_text(t: i64) -> String {
    let (d, h, m) = (t / 86400, t % 86400 / 3600, t % 3600 / 60);
    let mut s = String::new();
    if d > 0 {
        let _ = write!(s, "{d}d ");
    }
    if h > 0 {
        let _ = write!(s, "{h}h ");
    }
    let _ = write!(s, "{m}m");
    s
}
fn or_na(v: &str) -> Value {
    if v.is_empty() {
        json!("N/A")
    } else {
        number(lead(v))
    }
}
/// The `?ajax=1` reply without graphs (the browser draws them) and the history values.
pub fn build(i: &Inputs) -> (String, Record) {
    let mut record = Record::default();
    let trk = key_values(i.tracking);
    let chrony_ok = !trk.is_empty();
    let c = conf(i.conf);

    let frequency: Vec<&str> = lookup(&trk, "Frequency").split_whitespace().collect();
    let freq_abs = lead(frequency.first().unwrap_or(&""));
    let freq = if frequency.get(1) == Some(&"ppm") && frequency.get(2) == Some(&"slow") {
        -freq_abs
    } else {
        freq_abs
    };
    let proximity = (((freq_abs / c.max_drift * 1000.0 + 0.5).trunc()) / 10.0).min(100.0);
    let proximity_class = if proximity < 50.0 {
        "text-green"
    } else if proximity < 80.0 {
        "text-amber"
    } else {
        "text-red"
    };
    let drift_label = format!(
        "{} ppm{}",
        awk_number(c.max_drift),
        if c.drift_default { " (default)" } else { "" }
    );

    let (steps, step_line) = i.steps;
    let last_step = if steps > 0 {
        let magnitude = step_line
            .split_once("stepped by ")
            .map(|(_, rest)| {
                rest.chars()
                    .take_while(|c| c.is_ascii_digit() || "+-.e".contains(*c))
                    .collect::<String>()
            })
            .unwrap_or_default();
        let stamp: String = step_line.chars().take(15).collect();
        format!("{stamp} UTC · {magnitude} s")
    } else {
        "Never".into()
    };
    let steps_class = match steps {
        0 => "text-green",
        1 | 2 => "text-amber",
        _ => "text-red",
    };

    let mut sync = trk.clone();
    if chrony_ok {
        sync.push((
            "Clock Steps (24h)".into(),
            format!("<span class='{steps_class} font-bold'>{steps}</span>"),
        ));
        sync.push(("Last Step".into(), last_step));
        sync.push((
            "Freq Limit".into(),
            format!(
                "<span class='{proximity_class} font-bold'>{}%</span> of {drift_label}",
                awk_number(proximity)
            ),
        ));
        if i.chrony_up > 0 {
            let since = chrono::DateTime::from_timestamp(i.now - i.chrony_up, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            sync.push((
                "Chrony Uptime".into(),
                format!(
                    "<div class=\"has-popup\"><span class=\"tip-text tip-text-muted\">{}</span><div class=\"popup\">Chrony running since:<br><strong>{since} UTC</strong></div></div>",
                    uptime_text(i.chrony_up)
                ),
            ));
        }
    }

    let mut config = Vec::new();
    if let Some((threshold, limit)) = &c.step {
        let limit = if *limit == -1.0 {
            "always".to_owned()
        } else {
            format!(
                "first {} update{}",
                awk_number(*limit),
                if *limit != 1.0 { "s" } else { "" }
            )
        };
        config.push(("Make Step".into(), format!("{threshold}s - {limit}")));
    }
    config.push((
        "Max Drift".into(),
        format!(
            "{} ppm{}",
            awk_number(c.max_drift),
            if c.drift_default {
                " <span class='platform-meta'>(default)</span>"
            } else {
                ""
            }
        ),
    ));
    if let Some(poll) = &c.poll {
        config.push((
            "Refclock Poll".into(),
            format!(
                "{}s <span class=\"platform-meta\">(2^{poll})</span>",
                awk_number(2f64.powf(lead(poll)))
            ),
        ));
    }
    if let Some(precision) = &c.precision {
        config.push(("Ref Precision".into(), format!("{:.6}s", lead(precision))));
    }

    // GSA only names satellites, but a satellite can have a row per signal and the score
    // averages every row marked used. Mark only the row the front end shows: the strongest,
    // or the higher signal ID on a tie.
    let nmea = Nmea::parse(i.nmea);
    let mut use_row: HashMap<(u32, u32), usize> = HashMap::new();
    for (k, row) in nmea.sats.iter().enumerate() {
        if !nmea.used.contains(&row.used_key) {
            continue;
        }
        let better = use_row.get(&row.used_key).is_none_or(|&j| {
            let other = &nmea.sats[j];
            row.ss > other.ss || (row.ss == other.ss && row.sig > other.sig)
        });
        if better {
            use_row.insert(row.used_key, k);
        }
    }
    let mut sats = Vec::with_capacity(nmea.sats.len());
    let (mut n_all, mut sum_all, mut n_used, mut sum_used) = (0u32, 0.0, 0u32, 0.0);
    // Per PRN: strongest SNR, its constellation, used, and whether it is only a placeholder.
    let mut best: Vec<(u32, f64, u8, bool, bool)> = Vec::new();
    for (k, row) in nmea.sats.iter().enumerate() {
        let used = use_row.get(&row.used_key) == Some(&k);
        sats.push((
            format!("{}_{}_{}", row.prn, row.gid, row.sig),
            Sat {
                prn: row.prn,
                gnssid: row.gid,
                svid: row.svid,
                sigid: row.sig,
                el: number(row.el),
                az: number(row.az),
                ss: number(row.ss),
                used,
                health: used as u8,
                freqid: None,
                last_seen: i.now,
            },
        ));
        if row.ss > 0.0 {
            n_all += 1;
            sum_all += row.ss;
            if used {
                n_used += 1;
                sum_used += row.ss;
            }
        }
        let real = row.el != 0.0 || row.az != 0.0 || row.ss != 0.0 || used;
        match best.iter_mut().find(|b| b.0 == row.prn) {
            Some(b) => {
                if row.ss > b.1 {
                    b.1 = row.ss;
                    b.2 = row.prefix;
                }
                b.3 |= used;
                b.4 |= real;
            }
            None => best.push((row.prn, row.ss, row.prefix, used, real)),
        }
    }
    let seen = best.iter().filter(|b| b.4).count() as u16;
    let used_n = best.iter().filter(|b| b.4 && b.3).count() as u16;
    let snrhist: Vec<(u8, u32, f64)> = best
        .iter()
        .filter(|b| b.1 > 0.0)
        .map(|b| (b.2, b.0, b.1))
        .collect();

    let fix = match nmea.fix_mode.as_str() {
        "3" => "3D Fix",
        "2" => "2D Fix",
        _ => "No Fix",
    };
    let status = match nmea.quality.as_str() {
        "1" => "Normal",
        "2" => "DGPS",
        "4" => "RTK Fixed",
        "5" => "RTK Float",
        "6" => "DR",
        _ => "Unknown",
    };
    let model = if i.model.is_empty() {
        "Quectel module"
    } else {
        i.model
    };
    let live = Live {
        ts: i.now,
        sources: sources(i.sources),
        tracking: Tracking {
            sync: Ordered(sync),
            server: Ordered(key_values(i.serverstats)),
            config: Ordered(config),
        },
        gps: Gps {
            lat: nmea.lat.map_or(json!("N/A"), |v| fixed(v, 6)),
            lon: nmea.lon.map_or(json!("N/A"), |v| fixed(v, 6)),
            alt: if nmea.alt.is_empty() {
                json!("N/A")
            } else {
                fixed(lead(&nmea.alt), 1)
            },
            sats: Ordered(sats),
            fix,
            status,
            tdop: "N/A",
            hdop: or_na(&nmea.hdop),
            vdop: or_na(&nmea.vdop),
            pdop: or_na(&nmea.pdop),
            ept: "N/A",
            device: format!("{model} @ AT+QGPSGNMEA"),
            device_extended: format!(
                "Hardware: &nbsp;Quectel {model}\nFirmware: &nbsp;{}\nGPS Active Since: &nbsp;{}",
                i.firmware, i.started
            ),
            gpsd_version: "",
            gpsd_revision: "",
            gpsd_protocol: "",
            activated: i.started.into(),
        },
        service_status: Services {
            chrony: chrony_ok,
            gpsd: i.at_ok,
        },
        node: model.into(),
    };

    if chrony_ok {
        let first = |key: &str| lead(lookup(&trk, key));
        let system: Vec<&str> = lookup(&trk, "System time").split_whitespace().collect();
        let magnitude = lead(system.first().unwrap_or(&""));
        let system_offset = if system.get(2) == Some(&"slow") {
            -magnitude
        } else {
            magnitude
        };
        record.track = Some([
            freq,
            first("Skew"),
            first("Last offset"),
            first("Root dispersion"),
        ]);
        record.pps = Some([system_offset, first("RMS offset")]);
    }
    record.stats = source_stats(i.sourcestats);
    if i.at_ok && !nmea.sats.is_empty() {
        let average = |sum: f64, n: u32| if n > 0 { sum / n as f64 } else { 0.0 };
        record.sat = Some([seen, used_n]);
        record.snr = Some([average(sum_used, n_used), average(sum_all, n_all)]);
        record.snrhist = snrhist;
        if !nmea.pdop.is_empty() {
            record.dop = Some([lead(&nmea.hdop), lead(&nmea.vdop), lead(&nmea.pdop)]);
        }
    }
    (serde_json::to_string(&live).unwrap_or_default(), record)
}

// ---- history ----

const TRACK: u8 = 1;
const PPS: u8 = 2;
const STATS: u8 = 4;
const SAT: u8 = 8;
const SNR: u8 = 16;
const DOP: u8 = 32;
const SNRHIST: u8 = 64;
/// One 30 s history sample, about 80 bytes; 24 h of them is about 230 KB.
#[derive(Clone, Copy, Default)]
struct Sample {
    ts: u32,
    seq: u32,
    has: u8,
    track: [f32; 4],
    pps: [f32; 2],
    stats: [f32; 2],
    sat: [u16; 2],
    snr: [f32; 2],
    dop: [f32; 3],
    /// Average SNR of each constellation in hundredths of a dBHz, 0 when none was seen.
    constellations: [u16; 7],
}
struct Detail {
    ts: u32,
    sats: Vec<(u8, u16, f32)>,
}
#[derive(Default)]
struct History {
    samples: VecDeque<Sample>,
    detail: VecDeque<Detail>,
    seq: u32,
}
fn floats<const N: usize>(v: [f64; N]) -> [f32; N] {
    v.map(|x| x as f32)
}
impl History {
    fn push(&mut self, ts: i64, r: &Record) {
        let mut s = Sample {
            ts: ts.max(0) as u32,
            seq: self.seq,
            ..Default::default()
        };
        self.seq = self.seq.wrapping_add(1);
        if let Some(v) = r.track {
            s.has |= TRACK;
            s.track = floats(v);
        }
        if let Some(v) = r.pps {
            s.has |= PPS;
            s.pps = floats(v);
        }
        if let Some(v) = r.stats {
            s.has |= STATS;
            s.stats = floats(v);
        }
        if let Some(v) = r.sat {
            s.has |= SAT;
            s.sat = v;
        }
        if let Some(v) = r.snr {
            s.has |= SNR;
            s.snr = floats(v);
        }
        if let Some(v) = r.dop {
            s.has |= DOP;
            s.dop = floats(v);
        }
        if !r.snrhist.is_empty() {
            s.has |= SNRHIST;
            for (slot, letter) in CONSTELLATIONS.iter().enumerate() {
                let values: Vec<f64> = r
                    .snrhist
                    .iter()
                    .filter(|(l, _, _)| l == letter)
                    .map(|(_, _, v)| *v)
                    .collect();
                if !values.is_empty() {
                    let mean = values.iter().sum::<f64>() / values.len() as f64;
                    s.constellations[slot] = (mean * 100.0).round().clamp(1.0, 65535.0) as u16;
                }
            }
            self.detail.push_back(Detail {
                ts: s.ts,
                sats: r
                    .snrhist
                    .iter()
                    .map(|&(l, prn, v)| (l, prn.min(u16::MAX as u32) as u16, v as f32))
                    .collect(),
            });
            while self.detail.len() > DETAIL_SAMPLES {
                self.detail.pop_front();
            }
        }
        self.samples.push_back(s);
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }
    /// The newest `lines` samples of each series, every `step`th one, in the text format
    /// gnss-dash-poll wrote. Steps follow a sample counter, so a graph keeps its points
    /// as new samples arrive.
    fn bundle(&self, lines: usize, step: u32) -> String {
        let mut out = String::new();
        let series = [
            ("track", TRACK),
            ("stats", STATS),
            ("sat", SAT),
            ("snr", SNR),
            ("dop", DOP),
            ("pps", PPS),
            ("snrhist", SNRHIST),
        ];
        for (name, flag) in series {
            let _ = writeln!(out, "#{name}");
            if flag == SNRHIST && step == 1 && lines <= DETAIL_SAMPLES {
                let skip = self.detail.len().saturating_sub(lines);
                for d in self.detail.iter().skip(skip) {
                    let _ = write!(out, "{}", d.ts);
                    for (letter, prn, snr) in &d.sats {
                        let _ = write!(out, " {}{prn}:{snr:.2}", *letter as char);
                    }
                    out.push('\n');
                }
                continue;
            }
            let rows: Vec<&Sample> = self.samples.iter().filter(|s| s.has & flag != 0).collect();
            let skip = rows.len().saturating_sub(lines);
            for s in rows[skip..].iter().filter(|s| s.seq % step == 0) {
                let _ = match flag {
                    TRACK => writeln!(
                        out,
                        "{} {} {} {} {}",
                        s.ts, s.track[0], s.track[1], s.track[2], s.track[3]
                    ),
                    STATS => writeln!(out, "{} {} {}", s.ts, s.stats[0], s.stats[1]),
                    SAT => writeln!(out, "{} {} {}", s.ts, s.sat[0], s.sat[1]),
                    SNR => writeln!(out, "{} {:.2} {:.2}", s.ts, s.snr[0], s.snr[1]),
                    DOP => writeln!(out, "{} 0 {} {} {}", s.ts, s.dop[0], s.dop[1], s.dop[2]),
                    PPS => writeln!(out, "{} {} {}", s.ts, s.pps[0], s.pps[1]),
                    _ => {
                        // Longer windows carry each constellation's average under its letter.
                        let _ = write!(out, "{}", s.ts);
                        for (letter, centi) in CONSTELLATIONS.iter().zip(s.constellations) {
                            if centi > 0 {
                                let _ = write!(
                                    out,
                                    " {}:{:.2}",
                                    *letter as char,
                                    f64::from(centi) / 100.0
                                );
                            }
                        }
                        writeln!(out)
                    }
                };
            }
        }
        out
    }
}

// ---- syslog clock steps ----

/// Clock steps chronyd logged in the system log. Only new bytes are read each time; a
/// shorter file (rotated) is read again from the start.
#[derive(Default)]
struct Steps {
    offset: u64,
    count: usize,
    last: String,
    partial: Vec<u8>,
}
impl Steps {
    fn update(&mut self, path: &Path) {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut file) = std::fs::File::open(path) else {
            *self = Self::default();
            return;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = Self::default();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut data = std::mem::take(&mut self.partial);
        let Ok(read) = file.take(8 << 20).read_to_end(&mut data) else {
            return;
        };
        self.offset += read as u64;
        let mut rest = &data[..];
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&rest[..end]);
            if line.contains("System clock was stepped by") && line.contains("chronyd") {
                self.count += 1;
                self.last = line.into_owned();
            }
            rest = &rest[end + 1..];
        }
        // An unfinished last line waits for the rest; a runaway one is dropped.
        if rest.len() < 64 * 1024 {
            self.partial = rest.to_vec();
        }
    }
}

// ---- the collector ----

pub struct Gnss {
    chronyc: PathBuf,
    conf: PathBuf,
    mock: bool,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    installed: bool,
    live: Option<Arc<str>>,
    clients: Option<Arc<str>>,
    history: History,
    synced: bool,
    started: String,
    updated: i64,
}
/// What one collection cycle hands to the next.
#[derive(Default)]
pub struct Context {
    cycle: u64,
    steps: Steps,
    chronyd: Option<u32>,
    model: String,
    firmware: String,
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
/// Seconds the running chronyd has been up, 0 when it is not running.
fn chronyd_uptime(pid: &mut Option<u32>) -> i64 {
    let is_chronyd = |pid: &u32| {
        std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "chronyd")
    };
    if !pid.as_ref().is_some_and(is_chronyd) {
        *pid = std::fs::read_dir("/proc").ok().and_then(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
                .filter(is_chronyd)
                .min()
        });
    }
    let Some(pid) = *pid else { return 0 };
    let start = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            let fields = stat.rsplit_once(") ")?.1;
            fields.split_whitespace().nth(19)?.parse::<f64>().ok()
        });
    let uptime = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok());
    // SAFETY: sysconf only reads a configuration value.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let ticks = if ticks > 0 { ticks as f64 } else { 100.0 };
    match (start, uptime) {
        (Some(start), Some(uptime)) => (uptime - start / ticks) as i64,
        _ => 0,
    }
}
impl Gnss {
    pub fn new(config: &crate::Config) -> Arc<Self> {
        Arc::new(Self {
            chronyc: config.chronyc.clone(),
            conf: config.chrony_conf.clone(),
            mock: config.mock,
            state: Mutex::new(State::default()),
        })
    }
    fn installed(&self) -> bool {
        self.mock || self.chronyc.is_file()
    }
    pub fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({
            "installed": self.installed(),
            "synced": state.synced,
            "started": state.started,
            "updated": state.updated,
            "samples": state.history.samples.len(),
        })
    }
    /// One of the files the front end reads, with its content type.
    pub fn part(&self, name: &str) -> Option<(&'static str, String)> {
        let state = self.state.lock().unwrap();
        match name {
            "live" => Some(("application/json", state.live.as_deref()?.to_owned())),
            "clients" => Some((
                "application/json",
                state
                    .clients
                    .as_deref()
                    .unwrap_or("{\"clients\":[]}")
                    .to_owned(),
            )),
            "hist1" => Some(("text/plain; charset=utf-8", state.history.bundle(120, 1))),
            "hist6" => Some(("text/plain; charset=utf-8", state.history.bundle(720, 3))),
            "hist24" => Some((
                "text/plain; charset=utf-8",
                state.history.bundle(MAX_SAMPLES, 12),
            )),
            _ => None,
        }
    }
    pub fn start(self: &Arc<Self>, at: At) {
        let this = self.clone();
        tokio::spawn(async move {
            if this.mock {
                this.backfill(unix_now());
            }
            at.wait_ready().await;
            let mut context = Context::default();
            let mut tick = tokio::time::interval(PERIOD);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let installed = this.installed();
                this.state.lock().unwrap().installed = installed;
                if installed {
                    this.collect(&at, &mut context).await;
                }
            }
        });
    }
    async fn chronyc(&self, command: &str, now: i64) -> String {
        if self.mock {
            return mock::chrony(command, now);
        }
        let mut child = tokio::process::Command::new(&self.chronyc);
        child
            .arg("-n")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(5), child.output()).await {
            Ok(Ok(output)) => String::from_utf8_lossy(&output.stdout).into_owned(),
            _ => String::new(),
        }
    }
    /// The three NMEA replies, and whether every query was answered. A busy AT queue (a
    /// cell scan holds it for minutes) must not stall chrony's part of the page.
    async fn nmea(&self, at: &At) -> (String, bool) {
        let mut out = String::new();
        for command in NMEA {
            let reply = tokio::time::timeout(
                Duration::from_secs(4),
                at.transaction_timeout(command, None, Some(Duration::from_secs(3))),
            )
            .await;
            match reply {
                Ok(Ok(text)) => out.push_str(&text),
                _ => return (out, false),
            }
        }
        (out, true)
    }
    async fn identity(&self, at: &At, context: &mut Context) {
        if let Ok(raw) = at.page("model", false).await {
            let model = crate::parser::model(&raw);
            if model != "-" {
                context.model = model;
            }
        }
        if let Ok(raw) = at.fetch(crate::at::DEVICE_IDENTITY, false).await
            && let Some(firmware) = crate::parser::device(&raw)["firmwareVersion"].as_str()
            && firmware != "-"
        {
            context.firmware = firmware.into();
        }
    }
    /// One 10 s collection.
    pub async fn collect(&self, at: &At, context: &mut Context) {
        let mut now = unix_now();
        if context.cycle.is_multiple_of(LONG_EVERY) || context.model.is_empty() {
            self.identity(at, context).await;
        }
        let (nmea, at_ok) = self.nmea(at).await;
        let tracking = self.chronyc("tracking", now).await;
        let sources = self.chronyc("sources", now).await;
        let sourcestats = self.chronyc("sourcestats", now).await;
        let serverstats = self.chronyc("serverstats", now).await;
        let conf = if self.mock {
            include_str!("../tests/fixtures/gnss/chrony.conf").to_owned()
        } else {
            std::fs::read_to_string(&self.conf).unwrap_or_default()
        };
        let chrony_up = if self.mock {
            12300
        } else {
            chronyd_uptime(&mut context.chronyd)
        };
        if !self.mock {
            context.steps.update(Path::new(MESSAGES));
        }
        // The clock is months off at boot. A sample stamped before chrony's first sync would
        // stretch the graphs' time axis, so history starts after it.
        let (synced, started) = {
            let mut state = self.state.lock().unwrap();
            if !state.synced
                && !tracking.trim().is_empty()
                && !tracking.contains("Not synchronised")
            {
                state.synced = true;
                now = unix_now();
                state.started = chrono::DateTime::from_timestamp(now, 0)
                    .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                    .unwrap_or_default();
            }
            (state.synced, state.started.clone())
        };
        let hist = context.cycle.is_multiple_of(HIST_EVERY);
        let (live, record) = build(&Inputs {
            now,
            tracking: &tracking,
            serverstats: &serverstats,
            sources: &sources,
            sourcestats: &sourcestats,
            conf: &conf,
            nmea: &nmea,
            at_ok,
            steps: (context.steps.count, &context.steps.last),
            chrony_up,
            started: &started,
            model: &context.model,
            firmware: &context.firmware,
        });
        let clients_text = if hist {
            Some(clients(&self.chronyc("clients", now).await))
        } else {
            None
        };
        let mut state = self.state.lock().unwrap();
        state.live = Some(Arc::from(live));
        state.updated = now;
        if hist && synced {
            state.history.push(now, &record);
        }
        if let Some(text) = clients_text {
            state.clients = Some(Arc::from(text));
        }
        context.cycle += 1;
    }
    /// Six hours of made-up history, so a preview shows graphs straight away.
    fn backfill(&self, now: i64) {
        let nmea = mock::NMEA.concat();
        let mut state = self.state.lock().unwrap();
        let first = now - 6 * 3600;
        for ts in (first..now).step_by(30) {
            let tracking = mock::chrony("tracking", ts);
            let sourcestats = mock::chrony("sourcestats", ts);
            let (_, mut record) = build(&Inputs {
                now: ts,
                tracking: &tracking,
                serverstats: "",
                sources: "",
                sourcestats: &sourcestats,
                conf: "",
                nmea: &nmea,
                at_ok: true,
                steps: (0, ""),
                chrony_up: 0,
                started: "",
                model: "",
                firmware: "",
            });
            let wave = (ts as f64 / 1800.0).sin();
            if let Some(snr) = &mut record.snr {
                snr.iter_mut().for_each(|v| *v += 1.5 * wave);
            }
            for (letter, _, v) in &mut record.snrhist {
                *v += wave * if *letter == b'E' { 3.0 } else { 1.5 };
            }
            state.history.push(ts, &record);
        }
        state.synced = true;
        state.started = chrono::DateTime::from_timestamp(first, 0)
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_default();
    }
}

/// Preview data: chronyc replies that drift slowly, and NMEA from a real RM520N-GL.
mod mock {
    pub const NMEA: [&str; 3] = [
        include_str!("../tests/fixtures/gnss/gga.txt"),
        include_str!("../tests/fixtures/gnss/gsa.txt"),
        include_str!("../tests/fixtures/gnss/gsv.txt"),
    ];
    pub fn chrony(command: &str, now: i64) -> String {
        let t = now as f64;
        let offset = 1.4e-7 * (t / 300.0).sin() + 5e-8 * (t / 37.0).sin();
        let rms = 2.1e-7 + 4e-8 * (t / 900.0).sin();
        let freq = 15.33 + 0.02 * (t / 3600.0).sin();
        let system = 1.6e-8 + 1e-8 * (t / 53.0).sin();
        let reference = chrono::DateTime::from_timestamp(now - 7, 0)
            .map(|d| d.format("%a %b %d %H:%M:%S %Y").to_string())
            .unwrap_or_default();
        match command {
            "tracking" => format!(
                "Reference ID    : 50505300 (PPS)\nStratum         : 1\nRef time (UTC)  : {reference}\n\
                 System time     : {:.9} seconds {} of NTP time\nLast offset     : {offset:+.9} seconds\n\
                 RMS offset      : {rms:.9} seconds\nFrequency       : {freq:.3} ppm slow\n\
                 Residual freq   : +0.000 ppm\nSkew            : {:.3} ppm\n\
                 Root delay      : 0.000000001 seconds\nRoot dispersion : {:.9} seconds\n\
                 Update interval : 16.0 seconds\nLeap status     : Normal\n",
                system.abs(),
                if system < 0.0 { "slow" } else { "fast" },
                0.004 + 0.002 * (t / 700.0).sin().abs(),
                1.9e-5 + 2e-6 * (t / 400.0).sin(),
            ),
            "sources" => format!(
                "MS Name/IP address         Stratum Poll Reach LastRx Last sample\n\
                 ===============================================================================\n\
                 #* PPS                           0   4   377     7  {:+.0}ns[{:+.0}ns] +/- 1000ns\n",
                offset * 1e9,
                (offset + 1.3e-7) * 1e9
            ),
            "sourcestats" => format!(
                "Name/IP Address            NP  NR  Span  Frequency  Freq Skew  Offset  Std Dev\n\
                 ==============================================================================\n\
                 PPS                        16   9   240     -0.000      0.004  {:+5.0}ns  {:.0}ns\n",
                offset * 1e9,
                1000.0 + 120.0 * (t / 600.0).sin()
            ),
            "serverstats" => include_str!("../tests/fixtures/gnss/serverstats.txt").into(),
            "clients" => include_str!("../tests/fixtures/gnss/clients.txt").into(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gnss/");
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{DIR}{name}")).unwrap()
    }
    fn reference_build() -> (String, Record) {
        let nmea = fixture("gga.txt") + &fixture("gsa.txt") + &fixture("gsv.txt");
        let steps = fixture("steps.txt");
        build(&Inputs {
            now: 1793810968,
            tracking: &fixture("tracking.txt"),
            serverstats: &fixture("serverstats.txt"),
            sources: &fixture("sources.txt"),
            sourcestats: &fixture("sourcestats.txt"),
            conf: &fixture("chrony.conf"),
            nmea: &nmea,
            at_ok: true,
            steps: (1, steps.trim_end()),
            chrony_up: 1860,
            started: "2026-11-04T16:48:28Z",
            model: "RM520N-GL",
            firmware: "RM520NGLAAR03A04M4G_A0.303.A0.303",
        })
    }
    /// Series name to its lines, each split into fields.
    fn series(text: &str) -> HashMap<String, Vec<Vec<String>>> {
        let mut out: HashMap<String, Vec<Vec<String>>> = HashMap::new();
        let mut current = String::new();
        for line in text.lines() {
            if let Some(name) = line.strip_prefix('#') {
                current = name.into();
                out.entry(current.clone()).or_default();
            } else if !line.is_empty() {
                out.entry(current.clone())
                    .or_default()
                    .push(line.split(' ').map(String::from).collect());
            }
        }
        out
    }
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1e-12)
    }

    #[test]
    fn live_reply_matches_the_original_awk() {
        let (live, _) = reference_build();
        let mut ours: Value = serde_json::from_str(&live).unwrap();
        assert_eq!(ours["node"], "RM520N-GL");
        ours.as_object_mut().unwrap().remove("node");
        let expected: Value = serde_json::from_str(&fixture("expected-live.json")).unwrap();
        for key in ["ts", "sources", "tracking", "service_status"] {
            assert_eq!(ours[key], expected[key], "{key}");
        }
        let (gps, want) = (&ours["gps"], &expected["gps"]);
        for (key, value) in want.as_object().unwrap() {
            assert_eq!(&gps[key], value, "gps.{key}");
        }
        assert_eq!(
            gps.as_object().unwrap().len(),
            want.as_object().unwrap().len()
        );
        // The front end shows these in the order received; serde_json's Value sorts them.
        let expected_text = fixture("expected-live.json");
        for section in ["Sync Status", "Server Statistics", "NTP Configuration"] {
            let keys: Vec<&String> = expected["tracking"][section]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            let order = |text: &str| {
                let mut keys = keys.clone();
                keys.sort_by_key(|k| text.find(&format!("\"{k}\":")).unwrap());
                keys
            };
            assert_eq!(order(&live), order(&expected_text), "{section}");
        }
        let order = |text: &str| -> Vec<usize> {
            want["sats"]
                .as_object()
                .unwrap()
                .keys()
                .map(|k| text.find(&format!("\"{k}\":{{")).unwrap())
                .collect()
        };
        let (a, b) = (order(&live), order(&expected_text));
        let rank = |v: &[usize]| {
            let mut idx: Vec<usize> = (0..v.len()).collect();
            idx.sort_by_key(|&i| v[i]);
            idx
        };
        assert_eq!(rank(&a), rank(&b), "satellite order");
    }

    #[test]
    fn history_matches_the_original_awk() {
        let (_, record) = reference_build();
        let mut history = History::default();
        history.push(1793810968, &record);
        let ours = series(&history.bundle(120, 1));
        let expected = series(&fixture("expected-history.txt"));
        assert_eq!(ours.len(), 7);
        for (name, rows) in &expected {
            let got = &ours[name];
            assert_eq!(got.len(), rows.len(), "{name}");
            for (a, b) in got.iter().zip(rows) {
                if name == "snrhist" {
                    let set = |row: &[String]| {
                        let mut v: Vec<(String, f64)> = row[1..]
                            .iter()
                            .map(|e| {
                                let (k, v) = e.split_once(':').unwrap();
                                (k.to_owned(), v.parse().unwrap())
                            })
                            .collect();
                        v.sort_by(|x, y| x.0.cmp(&y.0));
                        v
                    };
                    assert_eq!(a[0], b[0]);
                    assert_eq!(set(a), set(b));
                    continue;
                }
                assert_eq!(a.len(), b.len(), "{name}: {a:?} {b:?}");
                for (x, y) in a.iter().zip(b) {
                    let (x, y): (f64, f64) = (x.parse().unwrap(), y.parse().unwrap());
                    assert!(close(x, y), "{name}: {x} != {y}");
                }
            }
        }
        // The 6 h and 24 h bundles carry constellation averages instead of satellites.
        let long = series(&history.bundle(720, 1));
        let row = &long["snrhist"][0];
        let averages: HashMap<&str, f64> = row[1..]
            .iter()
            .map(|e| {
                let (k, v) = e.split_once(':').unwrap();
                (k, v.parse().unwrap())
            })
            .collect();
        let gps: Vec<f64> = record
            .snrhist
            .iter()
            .filter(|s| s.0 == b'G')
            .map(|s| s.2)
            .collect();
        let mean = gps.iter().sum::<f64>() / gps.len() as f64;
        assert!((averages["G"] - mean).abs() < 0.006);
        assert_eq!(averages.len(), 5, "{row:?}");
    }

    #[test]
    fn clients_match_the_original_awk() {
        let ours: Value = serde_json::from_str(&clients(&fixture("clients.txt"))).unwrap();
        let expected: Value = serde_json::from_str(&fixture("expected-clients.json")).unwrap();
        assert_eq!(ours, expected);
        assert_eq!(clients(""), "{\"clients\":[]}");
    }

    #[test]
    fn awk_number_formatting() {
        for (x, s) in [
            (16.0, "16"),
            (0.25, "0.25"),
            (40.0, "40"),
            (0.3, "0.3"),
            (1.34e-7, "1.34e-07"),
            (123456.7, "123457"),
            (1234567.5, "1.23457e+06"),
            (-15.329, "-15.329"),
            (0.0001, "0.0001"),
        ] {
            assert_eq!(awk_number(x), s, "{x}");
        }
        assert_eq!(lead("+12ns"), 12.0);
        assert_eq!(lead(" -1.5e-3x"), -0.0015);
        assert_eq!(lead("abc"), 0.0);
        assert_eq!(lead("."), 0.0);
        assert!(close(seconds("1005ns"), 1.005e-6));
    }

    #[test]
    fn missing_or_partial_input_still_gives_a_page() {
        let empty = Inputs {
            now: 1,
            tracking: "",
            serverstats: "",
            sources: "",
            sourcestats: "",
            conf: "",
            nmea: "",
            at_ok: false,
            steps: (0, ""),
            chrony_up: 0,
            started: "",
            model: "",
            firmware: "",
        };
        let (live, record) = build(&empty);
        let live: Value = serde_json::from_str(&live).unwrap();
        assert_eq!(live["service_status"], json!({"chrony":false,"gpsd":false}));
        assert_eq!(live["gps"]["lat"], "N/A");
        assert_eq!(live["gps"]["fix"], "No Fix");
        assert_eq!(live["tracking"]["Sync Status"], json!({}));
        assert_eq!(
            live["tracking"]["NTP Configuration"]["Max Drift"],
            "500000 ppm <span class='platform-meta'>(default)</span>"
        );
        assert!(record.track.is_none() && record.sat.is_none() && record.snrhist.is_empty());

        // GNSS off: every query answers with an error, so the channel works but no satellites.
        let off = "AT+QGPSGNMEA=\"GGA\"\r\n+CME ERROR: 505\r\n";
        let (live, record) = build(&Inputs {
            nmea: off,
            at_ok: true,
            ..empty
        });
        let live: Value = serde_json::from_str(&live).unwrap();
        assert_eq!(live["service_status"]["gpsd"], true);
        assert_eq!(live["gps"]["sats"], json!({}));
        assert!(record.sat.is_none());
    }

    #[test]
    fn nmea_edge_cases() {
        // GGA without a fix keeps the first quality; GSV without a signal ID is L1;
        // satellite 0 and empty IDs are skipped; a repeated row replaces the earlier one.
        let text = "+QGPSGNMEA: $GNGGA,,,,,,0,00,99.9,,,,,,*48\r\n\
                    +QGPSGNMEA: $GPGGA,1,3117.0,N,12130.0,E,1,05,1.0,5.0,M,,,,*00\r\n\
                    +QGPSGNMEA: $GPGSV,1,1,03,01,10,100,30,00,10,10,10,,,,*00\r\n\
                    +QGPSGNMEA: $GPGSV,1,1,01,01,11,101,31*00\r\n\
                    +QGPSGNMEA: $GLGSV,1,1,01,70,20,200,,3*00\r\n\
                    +QGPSGNMEA: $GPGSV,1,1,01,40,30,150,35,1*00\r\n";
        let n = Nmea::parse(text);
        assert!(n.lat.is_none());
        assert_eq!(n.quality, "0");
        let rows: Vec<(u32, u8, u32, f64)> =
            n.sats.iter().map(|s| (s.prn, s.gid, s.sig, s.el)).collect();
        assert_eq!(rows, [(1, 0, 0, 11.0), (70, 6, 2, 20.0), (127, 1, 0, 30.0)]);
        // Southern and western hemispheres.
        let n = Nmea::parse("$GPGGA,1,3330.0,S,07030.0,W,1,05,1.0,500.5,M*00");
        assert_eq!(n.lat, Some(-33.5));
        assert_eq!(n.lon, Some(-70.5));
        assert_eq!(n.alt, "500.5");
    }

    #[test]
    fn history_windows_steps_and_limits() {
        let mut history = History::default();
        let record = Record {
            track: Some([1.0, 2.0, 3.0, 4.0]),
            sat: Some([10, 5]),
            snrhist: vec![(b'G', 1, 30.0), (b'R', 65, 40.0)],
            ..Default::default()
        };
        for i in 0..(MAX_SAMPLES as i64 + 100) {
            let mut r = record.clone();
            if i % 2 == 1 {
                r.sat = None;
            }
            history.push(1000 + i * 30, &r);
        }
        assert_eq!(history.samples.len(), MAX_SAMPLES);
        assert_eq!(history.detail.len(), DETAIL_SAMPLES);
        let one = series(&history.bundle(120, 1));
        assert_eq!(one["track"].len(), 120);
        assert_eq!(one["sat"].len(), 120);
        assert_eq!(one["stats"].len(), 0);
        assert_eq!(one["snrhist"].len(), 120);
        assert_eq!(one["snrhist"][0][1], "G1:30.00");
        // The sat series is in every other sample, so its 120 newest lines span 2 h.
        let span =
            one["sat"][119][0].parse::<i64>().unwrap() - one["sat"][0][0].parse::<i64>().unwrap();
        assert_eq!(span, 119 * 60);
        let day = series(&history.bundle(MAX_SAMPLES, 12));
        assert_eq!(day["track"].len(), MAX_SAMPLES / 12);
        assert_eq!(day["snrhist"][0][1..], ["G:30.00", "R:40.00"]);
        let newest = history.samples.back().unwrap().ts;
        // Points are picked by sample number, so they stay put as samples are added: only
        // the oldest may drop out.
        let before: Vec<String> = day["track"].iter().map(|r| r[0].clone()).collect();
        history.push(i64::from(newest) + 30, &record);
        let after: Vec<String> = series(&history.bundle(MAX_SAMPLES, 12))["track"]
            .iter()
            .map(|r| r[0].clone())
            .collect();
        let kept = before.iter().filter(|t| after.contains(t)).count();
        assert!(kept + 1 >= before.len(), "{kept} of {}", before.len());
    }

    #[test]
    fn clock_steps_are_read_incrementally_and_after_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("messages");
        let step = |n: u32| {
            format!(
                "Nov  {n} 16:20:15 host daemon.warn chronyd[1]: System clock was stepped by {n}.5 seconds\n"
            )
        };
        let mut steps = Steps::default();
        steps.update(&path);
        assert_eq!(steps.count, 0);
        std::fs::write(
            &path,
            format!(
                "noise\n{}other: System clock was stepped by 9 seconds\n",
                step(1)
            ),
        )
        .unwrap();
        steps.update(&path);
        assert_eq!(steps.count, 1);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let line = step(2);
        file.write_all(&line.as_bytes()[..20]).unwrap();
        steps.update(&path);
        assert_eq!(steps.count, 1);
        file.write_all(&line.as_bytes()[20..]).unwrap();
        steps.update(&path);
        assert_eq!(steps.count, 2);
        assert!(steps.last.contains("stepped by 2.5"));
        std::fs::write(&path, step(3)).unwrap();
        steps.update(&path);
        assert_eq!((steps.count, steps.last.contains("3.5")), (1, true));
        std::fs::remove_file(&path).unwrap();
        steps.update(&path);
        assert_eq!(steps.count, 0);
    }

    #[test]
    fn conf_reading() {
        let c = conf(
            "maxdrift 100\nmakestep 0.1 3\n# refclock poll 9\nrefclock PHC /dev/ptp0 poll -2 precision 1e-7\n",
        );
        assert_eq!(c.max_drift, 100.0);
        assert!(!c.drift_default);
        assert_eq!(c.step, Some(("0.1".into(), 3.0)));
        assert_eq!(c.poll.as_deref(), Some("-2"));
        assert_eq!(c.precision.as_deref(), Some("1e-7"));
    }

    #[tokio::test]
    async fn mock_collection_fills_every_part() {
        let mut config = <crate::Config as clap::Parser>::parse_from(["test", "--mock"]);
        config.chronyc = "/nonexistent/chronyc".into();
        let gnss = Gnss::new(&config);
        let at = At::start(true, vec![]).unwrap();
        assert!(gnss.part("live").is_none());
        gnss.backfill(1793810968);
        assert_eq!(gnss.snapshot()["samples"], 720);
        let mut context = Context::default();
        gnss.collect(&at, &mut context).await;
        let (kind, live) = gnss.part("live").unwrap();
        assert_eq!(kind, "application/json");
        let live: Value = serde_json::from_str(&live).unwrap();
        assert_eq!(live["service_status"], json!({"chrony":true,"gpsd":true}));
        assert_eq!(
            live["tracking"]["Sync Status"]["Reference ID"],
            "50505300 (PPS)"
        );
        assert!(live["gps"]["sats"].as_object().unwrap().len() > 50);
        assert_eq!(live["gps"]["fix"], "3D Fix");
        let clients: Value = serde_json::from_str(&gnss.part("clients").unwrap().1).unwrap();
        assert_eq!(clients["clients"].as_array().unwrap().len(), 3);
        let six = series(&gnss.part("hist6").unwrap().1);
        assert!(six["track"].len() >= 239, "{}", six["track"].len());
        assert!(gnss.part("hist24").is_some());
        assert!(gnss.part("other").is_none());
        assert_eq!(gnss.snapshot()["installed"], true);
    }
}
