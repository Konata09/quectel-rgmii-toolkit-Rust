use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn handle_proc(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 256];
    let _ = stream.read(&mut buf);
    let stat = fs::read_to_string("/proc/stat")
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let mem = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let up = fs::read_to_string("/proc/uptime").unwrap_or_default();
    let mut payload = String::with_capacity(1024);
    payload.push_str("===STAT===\n");
    payload.push_str(&stat);
    payload.push_str("\n===UPTIME===\n");
    payload.push_str(up.trim());
    payload.push_str("\n===MEMINFO===\n");
    for line in mem.lines().take(16) {
        payload.push_str(line);
        payload.push('\n');
    }
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        payload.len(),
        payload
    );
    let _ = stream.write_all(resp.as_bytes());
}

#[derive(Default)]
struct NmeaHub {
    clients: Vec<TcpStream>,
    latest_frame: Vec<String>,
}

fn is_standard_nmea(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 7 || b[0] != b'$' {
        return false;
    }
    matches!(
        &s[3..6],
        "GGA" | "GSA" | "GSV" | "RMC" | "VTG" | "ZDA"
    )
}

fn run_gnss_bridge(hub: Arc<Mutex<NmeaHub>>) {
    loop {
        let Ok(addr) = SocketAddr::from_abstract_name("mtk_gnssadp_at") else {
            thread::sleep(Duration::from_secs(3));
            continue;
        };
        let Ok(mut stream) = UnixStream::connect_addr(&addr) else {
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

        let init = concat!(
            "AT+GTGPSSWITCH=1\r\n",
            "AT+GTGPSCFG=2,6\r\n",
            "AT+GTGPSEPO=1\r\n",
            "AT+GTCFGNMEA=1\r\n",
            "AT+ELCSNMEACFG=1,63\r\n",
            "AT+GTGPSPOWER=1\r\n",
        );
        if stream.write_all(init.as_bytes()).is_err() {
            thread::sleep(Duration::from_secs(2));
            continue;
        }

        let mut reader = BufReader::new(stream);
        let mut current_frame: Vec<String> = Vec::new();
        let mut line = String::new();
        let mut silent_ticks = 0u32;

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Err(_) => {
                    silent_ticks += 1;
                    if silent_ticks >= 6 {
                        break;
                    }
                    continue;
                }
                Ok(_) => {
                    silent_ticks = 0;
                    let trimmed = line.trim();
                    let Some(tail) = trimmed.strip_prefix("+ELCSNMEA:") else {
                        continue;
                    };
                    let sentence = tail.trim().trim_matches('"').trim();
                    if !is_standard_nmea(sentence) {
                        continue;
                    }
                    let is_gga = &sentence[3..6] == "GGA";
                    let mut state = hub.lock().unwrap();
                    if is_gga && !current_frame.is_empty() {
                        state.latest_frame = std::mem::take(&mut current_frame);
                    }
                    current_frame.push(sentence.to_string());
                    let wire = format!("{sentence}\r\n");
                    state
                        .clients
                        .retain_mut(|client| client.write_all(wire.as_bytes()).is_ok());
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn run_nmea_server(hub: Arc<Mutex<NmeaHub>>) {
    let listener = TcpListener::bind("127.0.0.1:19091").expect("bind 127.0.0.1:19091");
    for mut stream in listener.incoming().flatten() {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = stream.set_nodelay(true);
        let mut state = hub.lock().unwrap();
        let mut ok = true;
        for line in &state.latest_frame {
            if stream.write_all(format!("{line}\r\n").as_bytes()).is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            if state.clients.len() >= 16 {
                state.clients.remove(0);
            }
            state.clients.push(stream);
        }
    }
}

fn main() {
    let hub = Arc::new(Mutex::new(NmeaHub::default()));
    let hub_gnss = Arc::clone(&hub);
    thread::spawn(move || run_gnss_bridge(hub_gnss));
    let hub_tcp = Arc::clone(&hub);
    thread::spawn(move || run_nmea_server(hub_tcp));

    let listener = TcpListener::bind("127.0.0.1:19090").expect("bind 127.0.0.1:19090");
    for stream in listener.incoming().flatten() {
        handle_proc(stream);
    }
}
