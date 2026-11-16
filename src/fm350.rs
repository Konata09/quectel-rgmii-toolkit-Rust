use crate::parser;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

fn resolve_tty_path(path: &str) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path))
}

/// Checks if `path` is a USB serial TTY belonging to a MediaTek/Fibocom FM350-GL (`0e8d:7127` / `0e8d:7126`).
/// Returns:
/// - `Some(true)` if it belongs to `0e8d` AND is interface `06` (`1.6`, the ONLY safe AT port).
/// - `Some(false)` if it belongs to `0e8d` AND is NOT interface `06` (UNSAFE port; sending AT crashes the modem).
/// - `None` if it is not a `0e8d` sysfs tty device.
pub fn classify_usb_tty(path: &str) -> Option<bool> {
    let resolved = resolve_tty_path(path);
    let name = resolved.file_name()?.to_str()?;
    if !name.starts_with("ttyUSB") && !name.starts_with("ttyACM") {
        return None;
    }
    let dev_dir = Path::new("/sys/class/tty").join(name).join("device");
    if !dev_dir.exists() {
        return None;
    }
    let vendor = std::fs::read_to_string(dev_dir.join("../../idVendor"))
        .or_else(|_| std::fs::read_to_string(dev_dir.join("../idVendor")))
        .ok()?;
    if vendor.trim() != "0e8d" {
        return None;
    }
    let iface = std::fs::read_to_string(dev_dir.join("../bInterfaceNumber"))
        .or_else(|_| std::fs::read_to_string(dev_dir.join("bInterfaceNumber")))
        .unwrap_or_default();
    Some(iface.trim() == "06")
}

/// Returns true if `path` is safe to open as an AT device.
/// Rejects non-interface-06 ports on Fibocom FM350-GL (`0e8d:7127`).
pub fn is_allowed_at_device(path: &str) -> bool {
    if classify_usb_tty(path) == Some(false) {
        return false;
    }
    let resolved = resolve_tty_path(path);
    let r = resolved.to_string_lossy();
    r == "/dev/smd11"
        || r.starts_with("/dev/ttyUSB")
        || r.starts_with("/dev/ttyACM")
        || r.starts_with("/dev/pts/")
        || r.starts_with("/dev/ttys")
        || r.starts_with("/dev/fm350")
}

/// Ensures the Linux `option` driver is bound to `0e8d:7127` (with ADB interface `:1.5` unbound)
/// and returns the safe AT port (`/dev/ttyUSB4` on interface `06`) if present.
pub fn discover_at_port() -> Option<String> {
    if let Some(port) = find_bound_at_port() {
        return Some(port);
    }
    if has_fm350_usb_device() {
        bind_option_driver();
        std::thread::sleep(Duration::from_millis(250));
        return find_bound_at_port();
    }
    None
}

fn find_bound_at_port() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/tty").ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("ttyUSB"))
        .collect();
    names.sort();
    for name in names {
        let dev = format!("/dev/{name}");
        if classify_usb_tty(&dev) == Some(true) {
            return Some(dev);
        }
    }
    None
}

fn has_fm350_usb_device() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else {
        return false;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let v = std::fs::read_to_string(p.join("idVendor")).unwrap_or_default();
        let prod = std::fs::read_to_string(p.join("idProduct")).unwrap_or_default();
        if v.trim() == "0e8d" && matches!(prod.trim(), "7127" | "7126") {
            return true;
        }
    }
    false
}

fn unbind_usb_interface_if_option(path: &Path, iface: &str) {
    let is_option = std::fs::read_link(path.join("driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy() == "option"))
        .unwrap_or(false);
    if is_option {
        let _ = std::fs::write("/sys/bus/usb/drivers/option/unbind", iface);
    }
}

fn bind_option_driver() {
    let _ = std::process::Command::new("modprobe")
        .arg("rndis_host")
        .status();
    let _ = std::process::Command::new("modprobe")
        .arg("option")
        .status();
    let _ = std::fs::write("/sys/bus/usb-serial/drivers/option1/new_id", "0e8d 7127\n");
    // Keep RNDIS (:1.0/:1.1) on rndis_host and ADB (:1.5) unbound from option.
    if let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") {
        let mut rndis_ctrls = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((_, suffix)) = name.rsplit_once(':') else {
                continue;
            };
            let v = std::fs::read_to_string(entry.path().join("../idVendor")).unwrap_or_default();
            if v.trim() != "0e8d" {
                continue;
            }
            if matches!(suffix, "1.0" | "1.1" | "1.5") {
                unbind_usb_interface_if_option(&entry.path(), &name);
            }
            if suffix == "1.0" {
                rndis_ctrls.push((entry.path(), name));
            }
        }
        for (path, iface) in rndis_ctrls {
            if !path.join("driver").exists() {
                let _ = std::fs::write("/sys/bus/usb/drivers/rndis_host/bind", iface);
            }
        }
    }
}

pub fn is_fm350_port(path: &str) -> bool {
    if std::env::var("SIMPLEADMIN_MODEM")
        .is_ok_and(|v| v.eq_ignore_ascii_case("fm350"))
    {
        return true;
    }
    classify_usb_tty(path) == Some(true)
}

/// Finds the host RNDIS network interface for the FM350-GL (e.g. `usb0` or `enx000011121314`).
pub fn rndis_interface() -> Option<String> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let vendor = std::fs::read_to_string(entry.path().join("device/../idVendor"))
                .unwrap_or_default();
            if vendor.trim() == "0e8d" {
                found.push(name);
            }
        }
    }
    if let Some(u) = found.iter().find(|n| n.as_str() == "usb0") {
        return Some(u.clone());
    }
    found.sort();
    found.into_iter().next()
}

/// Reads `(tx_bytes, rx_bytes)` from the FM350 RNDIS network interface on the host.
pub fn rndis_traffic_bytes() -> Option<(u64, u64)> {
    let iface = rndis_interface()?;
    let base = Path::new("/sys/class/net").join(iface).join("statistics");
    let tx: u64 = std::fs::read_to_string(base.join("tx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let rx: u64 = std::fs::read_to_string(base.join("rx_bytes"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((tx, rx))
}

#[cfg(target_os = "linux")]
fn send_ipv6_rs(iface: &str) {
    use std::ffi::CString;
    let Ok(ifname) = CString::new(iface) else {
        return;
    };
    let ifindex = unsafe { libc::if_nametoindex(ifname.as_ptr()) };
    if ifindex == 0 {
        return;
    }
    let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_RAW, libc::IPPROTO_ICMPV6) };
    if fd < 0 {
        return;
    }
    unsafe {
        let hops: libc::c_int = 255;
        libc::setsockopt(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_MULTICAST_HOPS,
            (&hops as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            ifname.as_ptr().cast(),
            (iface.len() + 1) as libc::socklen_t,
        );
        let mut dst: libc::sockaddr_in6 = std::mem::zeroed();
        dst.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        dst.sin6_addr.s6_addr = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        dst.sin6_scope_id = ifindex;
        let rs: [u8; 8] = [133, 0, 0, 0, 0, 0, 0, 0];
        libc::sendto(
            fd,
            rs.as_ptr().cast(),
            rs.len(),
            0,
            (&dst as *const libc::sockaddr_in6).cast(),
            std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        );
        libc::close(fd);
    }
}

#[cfg(not(target_os = "linux"))]
fn send_ipv6_rs(_iface: &str) {}

fn run_quiet(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn run_status_quiet(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

static LAST_RNDIS_SYNC: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

fn invalidate_rndis_cache() {
    *LAST_RNDIS_SYNC.lock().unwrap() = String::new();
}

/// Syncs host RNDIS interface IPv4/IPv6 addresses, neighbor entry, default routes, and DNS.
pub fn sync_rndis_network(raw: &str, ipv4: &str) {
    let Ok(addr) = ipv4.trim().parse::<std::net::Ipv4Addr>() else {
        return;
    };
    if addr.is_unspecified() || addr.is_loopback() {
        return;
    }
    let Some(iface) = rndis_interface() else {
        return;
    };

    let current_v4 = run_quiet("ip", &["-4", "-o", "addr", "show", "dev", &iface]);
    let cidr = format!("{addr}/24");
    let has_single_cidr =
        current_v4.lines().filter(|l| l.contains(" inet ")).count() == 1 && current_v4.contains(&cidr);
    let v4_route = run_quiet("ip", &["-4", "route", "show", "default", "dev", &iface]);
    let v4_ok = has_single_cidr && v4_route.contains("metric 101") && *LAST_RNDIS_SYNC.lock().unwrap() == cidr;

    if !v4_ok {
        let octets = addr.octets();
        let gw_last = if octets[3] == 1 { 2 } else { 1 };
        let gw4 = format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], gw_last);
        run_status_quiet("ip", &["link", "set", &iface, "up"]);
        run_status_quiet("ip", &["-4", "route", "del", "default", "dev", &iface]);
        run_status_quiet("ip", &["-4", "addr", "flush", "dev", &iface]);
        // Drop any stale global IPv6 prefix from an earlier PDN/RAT attach before triggering SLAAC.
        run_status_quiet("ip", &["-6", "addr", "flush", "dev", &iface, "scope", "global"]);
        run_status_quiet("ip", &["addr", "add", &cidr, "dev", &iface]);
        run_status_quiet(
            "ip",
            &["route", "replace", "default", "via", &gw4, "dev", &iface, "metric", "101"],
        );
        *LAST_RNDIS_SYNC.lock().unwrap() = cidr.clone();
    }

    // Ensure IPv6 SLAAC + permanent L3 M-RNDIS gateway neighbor & metric-101 route
    let sysctl_base = format!("/proc/sys/net/ipv6/conf/{iface}");
    let _ = std::fs::write(format!("{sysctl_base}/disable_ipv6"), "0");
    let _ = std::fs::write(format!("{sysctl_base}/accept_ra"), "1");
    let _ = std::fs::write(format!("{sysctl_base}/autoconf"), "1");
    let _ = std::fs::write(format!("{sysctl_base}/accept_ra_defrtr"), "1");

    let v6_addr = run_quiet("ip", &["-6", "-o", "addr", "show", "dev", &iface, "scope", "global"]);
    let v6_route = run_quiet("ip", &["-6", "route", "show", "default", "dev", &iface]);
    let v6_neigh = run_quiet("ip", &["-6", "neigh", "show", "dev", &iface]);
    let v6_ok = v4_ok
        && !v6_addr.trim().is_empty()
        && v6_route.contains("default via fe80::1")
        && v6_route.contains("metric 101")
        && v6_neigh.contains("fe80::1")
        && v6_neigh.contains("PERMANENT");

    if !v6_ok {
        if !v4_ok || v6_addr.trim().is_empty() {
            send_ipv6_rs(&iface);
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(120));
                if !run_quiet("ip", &["-6", "-o", "addr", "show", "dev", &iface, "scope", "global"])
                    .trim()
                    .is_empty()
                {
                    break;
                }
            }
        }
        run_status_quiet(
            "ip",
            &[
                "-6",
                "neigh",
                "replace",
                "fe80::1",
                "lladdr",
                "00:00:88:ff:00:00",
                "router",
                "dev",
                &iface,
                "nud",
                "permanent",
            ],
        );
        run_status_quiet(
            "ip",
            &[
                "-6",
                "route",
                "replace",
                "default",
                "via",
                "fe80::1",
                "dev",
                &iface,
                "metric",
                "101",
            ],
        );
    }

    if !v4_ok || !v6_ok {
        let mut dns = Vec::new();
        for line in parser::lines(raw) {
            if let Some(tail) = line.strip_prefix("+CGCONTRDP:") {
                let p = parser::fields(tail);
                if p.len() >= 6 && !is_ims_apn(&p[2]) {
                    for token in p.iter().skip(5).take(2) {
                        if let Some(ip) = super::parser::parse_pdp_ip(token)
                            && !dns.contains(&ip.to_string())
                        {
                            dns.push(ip.to_string());
                        }
                    }
                }
            }
        }
        if !dns.is_empty() {
            let mut args = vec!["dns", iface.as_str()];
            for d in &dns {
                args.push(d.as_str());
            }
            run_status_quiet("resolvectl", &args);
        }
    }
}

fn ensure_adb_forward(port: u16) -> bool {
    static LAST_FWD_TRY: LazyLock<Mutex<std::collections::HashMap<u16, Instant>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
    static ADB_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    let should_fwd = {
        let mut last = LAST_FWD_TRY.lock().unwrap();
        let ok = last
            .get(&port)
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
        if ok {
            last.insert(port, Instant::now());
        }
        ok
    };
    if !should_fwd {
        return false;
    }
    let _guard = ADB_LOCK.lock().unwrap();
    let spec = format!("tcp:{port}");
    std::process::Command::new("adb")
        .args(["forward", &spec, &spec])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Fetches `/proc/stat`, `/proc/meminfo`, and `/proc/uptime` from the FM350-GL module itself
/// via `fm350-agent` (`127.0.0.1:19090` forwarded over USB ADB).
pub fn read_module_proc_files() -> Option<(String, String, String)> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};

    if rndis_interface().is_none() && !has_fm350_usb_device() {
        return None;
    }

    let addr: SocketAddr = ([127, 0, 0, 1], 19090).into();
    let connect = || -> std::io::Result<TcpStream> {
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(80))?;
        let _ = s.set_read_timeout(Some(Duration::from_millis(120)));
        let _ = s.set_write_timeout(Some(Duration::from_millis(80)));
        Ok(s)
    };

    let mut stream = match connect() {
        Ok(s) => s,
        Err(_) => {
            if !ensure_adb_forward(19090) {
                return None;
            }
            connect().ok()?
        }
    };

    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&raw);

    let mut stat = String::new();
    let mut uptime = String::new();
    let mut meminfo = String::new();
    let mut section = "";
    for line in body.lines() {
        match line.trim() {
            "===STAT===" => section = "stat",
            "===UPTIME===" => section = "up",
            "===MEMINFO===" => section = "mem",
            _ => match section {
                "stat" => {
                    stat.push_str(line);
                    stat.push('\n');
                }
                "up" => {
                    uptime.push_str(line);
                    uptime.push('\n');
                }
                "mem" => {
                    meminfo.push_str(line);
                    meminfo.push('\n');
                }
                _ => {}
            },
        }
    }
    if stat.trim().is_empty() || meminfo.trim().is_empty() {
        return None;
    }
    Some((stat, meminfo, uptime))
}

/// Maps Quectel compound or single AT queries to their guaranteed-safe Fibocom FM350-GL base query.
pub fn translate_query(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed == crate::at::DASHBOARD {
        return Some(
            "AT+CPIN?;+CESQ;+GTSENRDTEMP=1;+GTDUALSIM?;+COPS?;+CEREG?;+C5GREG?;+GTCCINFO?;+GTCAINFO?;+CGCONTRDP;+EMBIND?".into(),
        );
    }
    if trimmed == crate::at::SIGNAL {
        return Some("AT+CESQ;+GTSENRDTEMP=1;+COPS?;+GTCCINFO?;+GTCAINFO?".into());
    }
    if trimmed == crate::at::DEVICE_IDENTITY {
        return Some("AT+CGMI;+CGSN;+CGMR;+GTPKGVER?".into());
    }
    match trimmed {
        "AT+QSIMSTAT?;+CPIN?;+QMAP=\"WWAN\"" => {
            Some("AT+CPIN?;+CGACT?;+CGCONTRDP;+EMBIND?".into())
        }
        "AT+QMAP=\"LANIP\"" | "AT+QMAP=\"WWAN\"" => Some("AT+CGACT?;+CGCONTRDP".into()),
        "AT+QUIMSLOT?;+QNWPREFCFG=\"mode_pref\";+QNWPREFCFG=\"nr5g_disable_mode\";+CGDCONT?;+CGCONTRDP=1;+QNWLOCK=\"common/4g\";+QNWLOCK=\"common/5g\"" => {
            Some("AT+GTDUALSIM?;+GTACT?;+E5GOPT?;+CGDCONT?;+CGCONTRDP;+EMMCHLCK?".into())
        }
        "AT+QCAINFO" | "AT+QENG=\"servingcell\";+QCAINFO" => {
            Some("AT+CEREG?;+C5GREG?;+GTCCINFO?;+GTCAINFO?".into())
        }
        "AT+QENG=\"servingcell\"" => Some("AT+CESQ;+GTCCINFO?".into()),
        "AT+QNWPREFCFG=\"lte_band\""
        | "AT+QNWPREFCFG=\"nr5g_band\""
        | "AT+QNWPREFCFG=\"nsa_nr5g_band\""
        | "AT+QNWPREFCFG=\"lte_band\";+QNWPREFCFG= \"nsa_nr5g_band\";+QNWPREFCFG= \"nr5g_band\""
        | "AT+QNWPREFCFG=\"lte_band\";+QNWPREFCFG=\"nsa_nr5g_band\";+QNWPREFCFG=\"nr5g_band\"" => {
            Some("AT+GTACT?;+GTACT=?".into())
        }
        "AT+QMAP=\"MPDN_RULE\";+QMAP=\"DHCPV6DNS\";+QCFG=\"usbnet\";+QMAP=\"DMZ\";+QMAP=\"DHCPV4DNS\"" => {
            Some("AT+GTUSBMODE?;+EMBIND?;+CGCONTRDP".into())
        }
        "AT+CGDCONT?;+CGACT?;+CGPADDR" => Some("AT+CGDCONT?;+CGACT?;+CGCONTRDP".into()),
        "AT+QCFG=\"ims\"" => Some("AT+CAVIMS?;+CASIMS?".into()),
        "AT+QSIMDET?;+QNWPREFCFG=\"roam_pref\";+QUIMSLOT?" => Some("AT+GTDUALSIM?".into()),
        "AT+QMBNCFG=\"AutoSel\";+QMBNCFG=\"list\"" => Some("AT+GTPKGVER?;+GTAPPVER?".into()),
        "AT+QNWPREFCFG=\"policy_band\"" => Some("AT+GTACT=?".into()),
        "AT+QENG=\"servingcell\";+QENG=\"neighbourcell\"" => Some("AT+CESQ;+GTCCINFO?;+ECELL".into()),
        "AT+QNWCFG=\"nr5g_meas_info\"" => Some("AT+GTCCINFO?;+ECELL".into()),
        "AT+QNWLOCK=\"common/4g\";+QNWLOCK=\"common/5g\""
        | "AT+QNWLOCK=\"common/4g\""
        | "AT+QNWLOCK=\"common/5g\""
        | "AT+QNWCFG=\"nr5g_earfcn_lock\"" => Some("AT+EMMCHLCK?".into()),
        "AT+QSCAN=3,1" | "AT+QSCAN=1,1" | "AT+QSCAN=2,1" => Some("AT+CESQ;+GTCCINFO?;+ECELL".into()),
        "AT+QGMR" => Some("AT+CGMR;+GTPKGVER?".into()),
        "AT+QTEMP" => Some("AT+GTSENRDTEMP=1".into()),
        "AT+QUIMSLOT?" => Some("AT+GTDUALSIM?".into()),
        "AT+CGPADDR" => Some("AT+CGCONTRDP".into()),
        _ => None,
    }
}

fn active_cids_from_raw(raw: &str) -> Vec<u32> {
    let mut cids = Vec::new();
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if matches!(key, "+CGCONTRDP" | "+CGACT") {
            let p = parser::fields(tail);
            if let Some(cid) = p.first().and_then(|s| s.parse::<u32>().ok())
                && (key == "+CGCONTRDP" || p.get(1).is_some_and(|st| st == "1"))
                && !cids.contains(&cid)
            {
                cids.push(cid);
            }
        }
    }
    cids
}

/// Runs a translated FM350-GL query and safely follows up with per-CID `+CGPADDR` or `+GTDNS`
/// queries so inactive CIDs never cause an `ERROR` abort inside a semicolon-chained command.
pub fn run_translated<F>(command: &str, mut run: F) -> Option<Result<String>>
where
    F: FnMut(&str) -> Result<String>,
{
    let base = translate_query(command)?;
    let trimmed = command.trim();
    Some((|| {
        let mut out = run(&base)?;
        let cids = active_cids_from_raw(&out);
        let need_pdp_addrs = trimmed == crate::at::DASHBOARD
            || trimmed == "AT+QSIMSTAT?;+CPIN?;+QMAP=\"WWAN\""
            || trimmed == "AT+QMAP=\"LANIP\""
            || trimmed == "AT+QMAP=\"WWAN\""
            || trimmed == "AT+CGDCONT?;+CGACT?;+CGPADDR"
            || trimmed == "AT+CGPADDR";
        if need_pdp_addrs {
            for cid in &cids {
                if let Ok(resp) = run(&format!("AT+CGPADDR={cid}"))
                    && parser::ok(&resp)
                {
                    out.push('\n');
                    out.push_str(&resp);
                }
            }
        }
        if trimmed == "AT+QMAP=\"MPDN_RULE\";+QMAP=\"DHCPV6DNS\";+QCFG=\"usbnet\";+QMAP=\"DMZ\";+QMAP=\"DHCPV4DNS\""
        {
            for cid in &cids {
                if let Ok(resp) = run(&format!("AT+GTDNS={cid}"))
                    && parser::ok(&resp)
                {
                    out.push('\n');
                    out.push_str(&resp);
                }
            }
        }
        Ok(out)
    })())
}

/// Checks if a command is a Quectel state-changing action that needs special execution on FM350-GL.
pub fn execute_action<F>(command: &str, mut run: F) -> Option<Result<String>>
where
    F: FnMut(&str) -> Result<String>,
{
    let trimmed = command.trim();
    let up = trimmed.to_ascii_uppercase();

    // 0. GNSS NMEA query: return live NMEA captured from FM350-GL GNSS (`fm350-agent` :19091).
    if up.starts_with("AT+QGPSGNMEA=") {
        if up.contains("\"GGA\"") {
            return Some(Ok(read_fm350_gnss_nmea()));
        }
        return Some(Ok("OK\r\n".into()));
    }

    // 1. IMEI write: AT+EGMR=1,7,"<imei>";+CFUN=1,1 -> AT+EGMREXT=1,7,"<imei>";+CFUN=1,1
    if up.starts_with("AT+EGMR=1,7,") {
        let translated = trimmed.replacen("AT+EGMR=1,7,", "AT+EGMREXT=1,7,", 1);
        return Some(run(&translated));
    }

    // 2. SIM slot switch: AT+QUIMSLOT=1|2 -> AT+GTDUALSIM=0|1
    if let Some(slot) = up.strip_prefix("AT+QUIMSLOT=") {
        let target = match slot.trim() {
            "1" => "0",
            "2" => "1",
            _ => return Some(Err(anyhow::anyhow!("invalid SIM slot"))),
        };
        return Some(run(&format!("AT+GTDUALSIM={target}")));
    }

    // 3. Cell / frequency lock & unlock
    if up == "AT+QNWLOCK=\"COMMON/4G\",0"
        || up == "AT+QNWLOCK=\"COMMON/5G\",0"
        || up == "AT+QNWCFG=\"NR5G_EARFCN_LOCK\",0"
        || up == "AT+QNWCFG=\"LTE_EARFCN_LOCK\",0"
    {
        return Some(run("AT+EMMCHLCK=0"));
    }
    if let Some(rest) = trimmed.strip_prefix("AT+QNWLOCK=\"common/4g\",") {
        let p = parser::fields(rest);
        if p.first().is_some_and(|n| n != "1") || p.len() != 3 {
            return Some(Err(anyhow::anyhow!("FM350-GL only supports locking 1 LTE cell")));
        }
        return Some(run(&format!("AT+EMMCHLCK=1,7,0,{},{}", p[1], p[2])));
    }
    if let Some(rest) = trimmed.strip_prefix("AT+QNWLOCK=\"common/5g\",") {
        let p = parser::fields(rest);
        if p.len() >= 2 {
            let pci = &p[0];
            let arfcn = &p[1];
            return Some(run(&format!("AT+EMMCHLCK=1,11,0,{arfcn},{pci}")));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("AT+QNWCFG=\"nr5g_earfcn_lock\",") {
        let p = parser::fields(rest);
        if p.first().is_some_and(|n| n != "1") {
            return Some(Err(anyhow::anyhow!("FM350-GL only supports locking 1 NR-ARFCN")));
        }
        if p.len() >= 2
            && let Some(arfcn) = p[1].split(':').next()
            && !arfcn.is_empty()
        {
            return Some(run(&format!("AT+EMMCHLCK=1,11,0,{arfcn}")));
        }
    }

    // 4. Reject Quectel-only network/USB/MBN features cleanly (no fake Ok stubs)
    if up.starts_with("AT+QCFG=\"USBNET\",")
        || up.starts_with("AT+QMAP=")
        || up.starts_with("AT+QMAPWAC=")
        || up.starts_with("AT+QSIMDET=")
        || up.starts_with("AT+QNWPREFCFG=\"ROAM_PREF\",")
        || up.starts_with("AT+QMBNCFG=")
    {
        return Some(Err(anyhow::anyhow!("feature is not supported on FM350-GL")));
    }

    // 5. IMS toggle: AT+QCFG="ims",<0|1> -> AT+CAVIMS=<0|1>
    if let Some(mode) = up.strip_prefix("AT+QCFG=\"IMS\",") {
        let v = match mode.trim() {
            "0" => 0,
            "1" => 1,
            _ => return Some(Err(anyhow::anyhow!("FM350-GL IMS only supports 0 (off) or 1 (on)"))),
        };
        return Some(run(&format!("AT+CAVIMS={v}")));
    }

    // 6. PDP activate: AT+CGACT=1,<cid> -> activate, bind M-RNDIS (data CID >= 2), and sync host IPv4/IPv6
    if let Some(cid_str) = up.strip_prefix("AT+CGACT=1,")
        && let Ok(cid) = cid_str.trim().parse::<u32>()
    {
        return Some((|| {
            let out = run(&format!("AT+CGACT=1,{cid}"))?;
            if parser::ok(&out) && cid >= 2 {
                bind_and_sync_cid(&mut run, cid);
            }
            Ok(out)
        })());
    }

    // 8. Band lock / network mode settings (`+QNWPREFCFG=...`)
    if up.contains("+QNWPREFCFG=") {
        return Some((|| {
            let mut cgdcont_cmds: Vec<(u32, String)> = Vec::new();
            let mut mode_pref: Option<String> = None;
            let mut nr_disable: Option<u32> = None;
            let mut lte_bands: Option<Vec<u32>> = None;
            let mut nr_bands: Option<Vec<u32>> = None;

            for part in crate::at::split(trimmed) {
                let pup = part.to_ascii_uppercase();
                if pup.starts_with("AT+CGDCONT=") {
                    // On FM350-GL, CID 1 holds IMS while CID 2 carries M-RNDIS data; target CID 2 directly.
                    if let Some(rest) = part
                        .strip_prefix("AT+CGDCONT=1,")
                        .or_else(|| pup.strip_prefix("AT+CGDCONT=1,").map(|r| &part[part.len() - r.len()..]))
                    {
                        cgdcont_cmds.push((2, format!("AT+CGDCONT=2,{rest}")));
                    } else {
                        let cid = part
                            .get("AT+CGDCONT=".len()..)
                            .and_then(|s| s.split(',').next())
                            .and_then(|s| s.trim().parse::<u32>().ok())
                            .unwrap_or(2);
                        cgdcont_cmds.push((cid, part.clone()));
                    }
                } else if let Some(idx) = pup.find("+QNWPREFCFG=") {
                    let tail = &part[idx + "+QNWPREFCFG=".len()..];
                    let f = parser::fields(tail);
                    if f.len() >= 2 {
                        match f[0].to_ascii_lowercase().as_str() {
                            "mode_pref" => mode_pref = Some(f[1].to_ascii_uppercase()),
                            "nr5g_disable_mode" => nr_disable = f[1].parse().ok(),
                            "lte_band" => {
                                lte_bands = Some(
                                    f[1].split(':')
                                        .filter_map(|s| s.parse::<u32>().ok())
                                        .collect(),
                                );
                            }
                            "nr5g_band" | "nsa_nr5g_band" => {
                                nr_bands = Some(
                                    f[1].split(':')
                                        .filter_map(|s| s.parse::<u32>().ok())
                                        .collect(),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }

            let mut last_out = "\r\nOK\r\n".to_string();
            let will_reattach = mode_pref.is_some() || nr_disable.is_some();
            for (cid, cmd) in cgdcont_cmds {
                last_out = run(&cmd)?;
                if !parser::ok(&last_out) {
                    let _ = run(&format!("AT+CGACT=0,{cid}"));
                    last_out = run(&cmd)?;
                }
                if parser::ok(&last_out) && cid >= 2 && !will_reattach {
                    let _ = run(&format!("AT+CGACT=0,{cid}"));
                    if let Ok(act) = run(&format!("AT+CGACT=1,{cid}"))
                        && parser::ok(&act)
                    {
                        bind_and_sync_cid(&mut run, cid);
                    }
                }
            }

            if let Some(dis) = nr_disable {
                let opt = match dis {
                    1 => 5, // LTE + Option 3 (NSA), disable SA
                    2 => 3, // LTE + Option 2 (SA), disable NSA
                    _ => 7, // LTE + SA + NSA
                };
                last_out = run(&format!("AT+E5GOPT={opt}"))?;
            }

            if mode_pref.is_some() || lte_bands.is_some() || nr_bands.is_some() {
                let current = run("AT+GTACT?").unwrap_or_default();
                let caps = if lte_bands.is_none() && nr_bands.is_none() {
                    // Fill missing target band families when switching out of a single-RAT mode.
                    run("AT+GTACT=?").unwrap_or_default()
                } else {
                    String::new()
                };
                let gtact_cmd = build_gtact_command_with_caps(
                    &current,
                    &caps,
                    mode_pref.as_deref(),
                    nr_disable,
                    lte_bands.as_deref(),
                    nr_bands.as_deref(),
                );
                last_out = run(&gtact_cmd)?;
            }
            Ok(last_out)
        })());
    }

    None
}

fn bind_and_sync_cid<F>(run: &mut F, cid: u32)
where
    F: FnMut(&str) -> Result<String>,
{
    let _ = run(&format!("AT+EMBIND=1,\"M-RNDIS\",{cid}"));
    invalidate_rndis_cache();
    if let Ok(addr_raw) = run(&format!("AT+CGCONTRDP={cid};+CGPADDR={cid}")) {
        let ims_cids = ims_cids_from_raw(&addr_raw);
        if let (Some(v4), _) = extract_data_ipv4_ipv6(&addr_raw, &ims_cids) {
            sync_rndis_network(&addr_raw, &v4);
        }
    }
}

fn parse_gtact_caps(caps_raw: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    for line in parser::lines(caps_raw) {
        if let Some(tail) = line.strip_prefix("+GTACT:")
            && tail.contains('(')
        {
            let groups: Vec<&str> = tail
                .split('(')
                .skip(1)
                .filter_map(|s| s.split(')').next())
                .collect();
            let parse_group = |idx: usize, min: u32, max: u32| -> Vec<String> {
                groups
                    .get(idx)
                    .map(|g| {
                        g.split(',')
                            .map(str::trim)
                            .filter(|s| s.parse::<u32>().is_ok_and(|n| (min..=max).contains(&n)))
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default()
            };
            return (
                parse_group(4, 1, 10),
                parse_group(5, 101, 199),
                parse_group(8, 501, 9999),
            );
        }
    }
    (Vec::new(), Vec::new(), Vec::new())
}

fn build_gtact_command_with_caps(
    current_raw: &str,
    caps_raw: &str,
    mode_pref: Option<&str>,
    nr_disable: Option<u32>,
    new_lte: Option<&[u32]>,
    new_nr: Option<&[u32]>,
) -> String {
    let mut rat = 20u32;
    let mut pref1 = "6".to_string();
    let mut pref2 = "3".to_string();
    let mut umts: Vec<String> = vec!["1".into(), "2".into(), "4".into(), "5".into(), "8".into()];
    let mut lte: Vec<String> = Vec::new();
    let mut nr: Vec<String> = Vec::new();

    for line in parser::lines(current_raw) {
        if let Some(tail) = line.strip_prefix("+GTACT:")
            && !tail.contains('(')
        {
            let p = parser::fields(tail);
            if p.len() >= 3 {
                rat = p[0].parse().unwrap_or(20);
                pref1 = p[1].clone();
                pref2 = p[2].clone();
                umts.clear();
                for token in p.iter().skip(3) {
                    if let Ok(code) = token.parse::<u32>() {
                        if (1..=10).contains(&code) {
                            umts.push(token.clone());
                        } else if (101..=199).contains(&code) {
                            lte.push(token.clone());
                        } else if code >= 501 {
                            nr.push(token.clone());
                        }
                    }
                }
            }
        }
    }

    if let Some(mode) = mode_pref {
        match mode {
            "LTE" => {
                rat = 2;
                pref1 = "3".into();
                pref2 = "3".into();
            }
            "NR5G" => {
                rat = 14;
                pref1 = "6".into();
                pref2 = "6".into();
            }
            "LTE:NR5G" => {
                rat = 17;
                pref1 = "6".into();
                pref2 = "3".into();
            }
            _ => {
                rat = 20;
                pref1 = "6".into();
                pref2 = "3".into();
            }
        }
    }
    if let Some(dis) = nr_disable {
        match dis {
            1 if rat == 14 => {
                // Disable SA -> LTE or NSA
                rat = 17;
                pref1 = "6".into();
                pref2 = "3".into();
            }
            2 if rat == 17 && mode_pref == Some("NR5G") => {
                // Disable NSA -> SA/LTE
                rat = 14;
                pref1 = "6".into();
                pref2 = "6".into();
            }
            _ => {}
        }
    }

    if !caps_raw.is_empty() {
        let (cap_umts, cap_lte, cap_nr) = parse_gtact_caps(caps_raw);
        if umts.is_empty() {
            umts = cap_umts;
        }
        if lte.is_empty() {
            lte = cap_lte;
        }
        if nr.is_empty() {
            nr = cap_nr;
        }
    }

    if let Some(l) = new_lte {
        lte = l.iter().map(|b| (100 + b).to_string()).collect();
    }
    if let Some(n) = new_nr {
        nr = n.iter().map(|b| format!("50{b}")).collect();
    }

    let mut args = vec![
        rat.to_string(),
        if pref1.is_empty() { "6".into() } else { pref1 },
        if pref2.is_empty() { "3".into() } else { pref2 },
    ];
    if matches!(rat, 1 | 4 | 16 | 20) {
        args.extend(umts);
    }
    if matches!(rat, 2 | 4 | 17 | 20) {
        args.extend(lte);
    }
    if matches!(rat, 14 | 16 | 17 | 20) {
        args.extend(nr);
    }
    format!("AT+GTACT={}", args.join(","))
}

fn extract_embind_rndis_cid(raw: &str) -> Option<u32> {
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("+EMBIND:") {
            let p = parser::fields(tail);
            if p.len() >= 3
                && p[0] == "1"
                && p[1].eq_ignore_ascii_case("M-RNDIS")
                && let Ok(cid) = p[2].parse::<u32>()
            {
                return Some(cid);
            }
        }
    }
    None
}

/// Ensures an active data PDP context (>= 2) and `M-RNDIS` binding exist when the FM350-GL is registered.
pub fn ensure_data_session<F>(dashboard_raw: &mut String, run: F)
where
    F: FnMut(&str) -> Result<String>,
{
    static LAST_ATTEMPT: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));
    ensure_data_session_with_throttle(dashboard_raw, &LAST_ATTEMPT, run);
}

fn ensure_data_session_with_throttle<F>(
    dashboard_raw: &mut String,
    last_attempt: &Mutex<Option<Instant>>,
    mut run: F,
) where
    F: FnMut(&str) -> Result<String>,
{
    if !dashboard_raw.contains("+CPIN: READY") {
        return;
    }
    let reg_ok = |prefix: &str| {
        parser::lines(dashboard_raw).any(|l| {
            l.strip_prefix(prefix)
                .map(parser::fields)
                .is_some_and(|p| p.get(1).or_else(|| p.first()).is_some_and(|s| matches!(s.as_str(), "1" | "5")))
        })
    };
    let registered = reg_ok("+CEREG:")
        || reg_ok("+C5GREG:")
        || reg_ok("+CGREG:")
        || gtccinfo_rows(dashboard_raw)
            .iter()
            .any(|r| r[0] == "1" && !r[2].trim().is_empty());
    if !registered {
        return;
    }

    let ims_cids = ims_cids_from_raw(dashboard_raw);
    let bound_rndis_cid = extract_embind_rndis_cid(dashboard_raw);

    // Only CIDs >= 2 (non-IMS) can be bound via AT+EMBIND (`+EMBIND` rejects CID 0 and CID 1 is IMS).
    let mut active_bindable: Option<(u32, String)> = None;
    for line in parser::lines(dashboard_raw) {
        if let Some(tail) = line.strip_prefix("+CGPADDR:") {
            let p = parser::fields(tail);
            if p.len() >= 2
                && !ims_cids.contains(&p[0])
                && let Ok(cid) = p[0].parse::<u32>()
                && cid >= 2
                && p[1].split('.').count() == 4
                && p[1] != "0.0.0.0"
            {
                active_bindable = Some((cid, p[1].clone()));
                if bound_rndis_cid == Some(cid) {
                    break;
                }
            }
        } else if let Some(tail) = line.strip_prefix("+CGCONTRDP:") {
            let p = parser::fields(tail);
            if p.len() > 3
                && !is_ims_apn(&p[2])
                && let Ok(cid) = p[0].parse::<u32>()
                && cid >= 2
                && p[3].split('.').count() == 4
                && p[3] != "0.0.0.0"
                && active_bindable.is_none()
            {
                active_bindable = Some((cid, p[3].clone()));
            }
        }
    }

    if let Some((cid, ip)) = active_bindable {
        if bound_rndis_cid == Some(cid) {
            sync_rndis_network(dashboard_raw, &ip);
            return;
        }
        {
            let mut last = last_attempt.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
                return;
            }
            *last = Some(Instant::now());
        }
        if run(&format!("AT+EMBIND=1,\"M-RNDIS\",{cid}")).is_ok_and(|r| parser::ok(&r)) {
            invalidate_rndis_cache();
            sync_rndis_network(dashboard_raw, &ip);
        }
        return;
    }

    // Throttle auto-dial attempts to once every 10 seconds.
    {
        let mut last = last_attempt.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
            return;
        }
        *last = Some(Instant::now());
    }

    // Preserve any existing CID 2 definition; otherwise populate from CID 0 or PLMN default.
    let cgdcont_raw = run("AT+CGDCONT?").unwrap_or_default();
    let mut cid2_defined = false;
    let mut cid0_apn: Option<String> = None;
    for line in parser::lines(&cgdcont_raw) {
        if let Some(tail) = line.strip_prefix("+CGDCONT:") {
            let p = parser::fields(tail);
            if p.len() >= 3 {
                let apn = p[2].trim();
                if p[0] == "2" && ["IP", "IPV6", "IPV4V6"].contains(&p[1].as_str()) && !is_ims_apn(apn) {
                    cid2_defined = true;
                } else if p[0] == "0" && !apn.is_empty() && !is_ims_apn(apn) {
                    cid0_apn = Some(strip_3gpp_apn_suffix(apn).to_string());
                }
            }
        }
    }
    let fallback_apn = cid0_apn.unwrap_or_else(|| detect_default_apn(dashboard_raw).to_string());
    if !cid2_defined {
        let _ = run(&format!("AT+CGDCONT=2,\"IPV4V6\",\"{fallback_apn}\""));
    }

    let mut activated = run("AT+CGACT=1,2").is_ok_and(|r| parser::ok(&r));
    if !activated && cid2_defined {
        // Fallback if CID 2 was defined with an empty/rejected APN.
        let _ = run(&format!("AT+CGDCONT=2,\"IPV4V6\",\"{fallback_apn}\""));
        activated = run("AT+CGACT=1,2").is_ok_and(|r| parser::ok(&r));
    }

    if activated {
        if bound_rndis_cid != Some(2) {
            let _ = run("AT+EMBIND=1,\"M-RNDIS\",2");
        }
        invalidate_rndis_cache();
        if let Ok(extra) = run("AT+CGCONTRDP=2;+CGPADDR=2") {
            dashboard_raw.push('\n');
            dashboard_raw.push_str(&extra);
            for line in parser::lines(&extra) {
                if let Some(tail) = line.strip_prefix("+CGPADDR:") {
                    let p = parser::fields(tail);
                    if p.first().is_some_and(|c| c == "2")
                        && let Some(ip) = p.get(1)
                    {
                        sync_rndis_network(dashboard_raw, ip);
                    }
                }
            }
        }
    }
}

fn detect_default_apn(raw: &str) -> &'static str {
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("+COPS:") {
            let p = parser::fields(tail);
            if let Some(plmn) = p.get(2) {
                return apn_for_plmn(plmn);
            }
        }
    }
    "3gnet"
}

/// Starts background FM350-GL GNSS backup thread (ADB `:19091` NMEA -> chrony `SOCK`).
pub fn start_gnss_backup() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("fm350-nmea".into())
            .spawn(run_fm350_nmea_stream);
    });
}

fn parse_rmc_utc(line: &str) -> Option<f64> {
    let body = line.trim().strip_prefix('$')?.split('*').next()?;
    let f: Vec<&str> = body.split(',').collect();
    if f.len() < 10 || !f[0].ends_with("RMC") || f[2] != "A" {
        return None;
    }
    let t = f[1];
    let d = f[9];
    if t.len() < 6 || d.len() != 6 {
        return None;
    }
    let hh: u32 = t[0..2].parse().ok()?;
    let mm: u32 = t[2..4].parse().ok()?;
    let ss: u32 = t[4..6].parse().ok()?;
    let nanos: u32 = if let Some(frac) = t.get(6..).and_then(|s| s.strip_prefix('.')) {
        let padded = format!("{frac:0<9}");
        padded[..9].parse().ok()?
    } else {
        0
    };
    let dd: u32 = d[0..2].parse().ok()?;
    let mo: u32 = d[2..4].parse().ok()?;
    let yy: i32 = d[4..6].parse().ok()?;
    let dt = chrono::NaiveDate::from_ymd_opt(2000 + yy, mo, dd)?
        .and_hms_nano_opt(hh, mm, ss, nanos)?
        .and_utc();
    Some(dt.timestamp_nanos_opt()? as f64 / 1e9)
}

fn build_chrony_sock_packet(tv_sec: i64, tv_usec: i64, offset: f64) -> [u8; 40] {
    let mut pkt = [0u8; 40];
    pkt[0..8].copy_from_slice(&tv_sec.to_ne_bytes());
    pkt[8..16].copy_from_slice(&tv_usec.to_ne_bytes());
    pkt[16..24].copy_from_slice(&offset.to_ne_bytes());
    // pulse = 0 (bytes 24..28), leap = 0 (bytes 28..32), _pad = 0 (bytes 32..36)
    pkt[36..40].copy_from_slice(&0x534f_434b_i32.to_ne_bytes());
    pkt
}

fn feed_chrony_sock(rmc_utc: f64) {
    use std::os::unix::net::UnixDatagram;
    use std::time::UNIX_EPOCH;

    let Ok(dur) = std::time::SystemTime::now().duration_since(UNIX_EPOCH) else {
        return;
    };
    let tv_sec = dur.as_secs() as i64;
    let tv_usec = dur.subsec_micros() as i64;
    let recv_unix = tv_sec as f64 + (tv_usec as f64 / 1e6);
    let delay = recv_unix - rmc_utc;
    // Drop stale replay frames on initial TCP connect.
    if !(-0.5..=1.5).contains(&delay) {
        return;
    }
    let pkt = build_chrony_sock_packet(tv_sec, tv_usec, rmc_utc - recv_unix);
    if let Ok(sock) = UnixDatagram::unbound() {
        let _ = sock.send_to(&pkt, "/run/chrony/fm350.sock");
    }
}

static FM350_NMEA_FRAME: LazyLock<Mutex<(Option<Instant>, String)>> =
    LazyLock::new(|| Mutex::new((None, String::new())));

fn run_fm350_nmea_stream() {
    use std::io::{BufRead, BufReader};
    use std::net::{SocketAddr, TcpStream};

    let addr: SocketAddr = ([127, 0, 0, 1], 19091).into();
    let mut last_rmc_utc = 0.0_f64;

    loop {
        if rndis_interface().is_none() && !has_fm350_usb_device() {
            std::thread::sleep(Duration::from_secs(5));
            continue;
        }
        let stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
            Ok(s) => s,
            Err(_) => {
                let _ = ensure_adb_forward(19091);
                match TcpStream::connect_timeout(&addr, Duration::from_millis(400)) {
                    Ok(s) => s,
                    Err(_) => {
                        std::thread::sleep(Duration::from_secs(5));
                        continue;
                    }
                }
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = BufReader::new(stream);
        let mut burst: Vec<String> = Vec::new();
        let mut seen_gga = false;
        let mut live_ticks = 0u32;
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let t = line.trim();
                    if !t.starts_with('$') || t.len() < 6 {
                        continue;
                    }
                    let kind = &t[3..6];
                    if kind == "RMC"
                        && live_ticks >= 1
                        && let Some(rmc_utc) = parse_rmc_utc(t)
                        && rmc_utc > last_rmc_utc + 0.2
                    {
                        last_rmc_utc = rmc_utc;
                        feed_chrony_sock(rmc_utc);
                    }
                    if kind == "GGA" {
                        if seen_gga && !burst.is_empty() {
                            let mut qgps = String::with_capacity(1024);
                            for s in &burst {
                                qgps.push_str("+QGPSGNMEA: ");
                                qgps.push_str(s);
                                qgps.push_str("\r\n");
                            }
                            qgps.push_str("OK\r\n");
                            *FM350_NMEA_FRAME.lock().unwrap() = (Some(Instant::now()), qgps);
                            burst.clear();
                            live_ticks = live_ticks.saturating_add(1);
                        }
                        seen_gga = true;
                    }
                    if seen_gga {
                        burst.push(t.to_owned());
                    }
                }
            }
        }
        if live_ticks == 0 {
            let _ = ensure_adb_forward(19091);
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn read_fm350_gnss_nmea() -> String {
    start_gnss_backup();
    let st = FM350_NMEA_FRAME.lock().unwrap();
    if st.0.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) && !st.1.is_empty() {
        return st.1.clone();
    }
    "OK\r\n".into()
}

/// Strips the 3GPP PLMN operator suffix (`.MNCxxx.MCCxxx.GPRS`) from dynamic `+CGCONTRDP` APNs.
pub fn strip_3gpp_apn_suffix(apn: &str) -> &str {
    let apn = apn.trim();
    let Some((rest, last)) = apn.rsplit_once('.') else {
        return apn;
    };
    if !last.eq_ignore_ascii_case("gprs") {
        return apn;
    }
    let Some((rest, mcc)) = rest.rsplit_once('.') else {
        return apn;
    };
    let Some(mcc_digits) = mcc
        .get(..3)
        .filter(|p| p.eq_ignore_ascii_case("mcc"))
        .and_then(|_| mcc.get(3..))
    else {
        return apn;
    };
    if mcc_digits.is_empty() || !mcc_digits.bytes().all(|b| b.is_ascii_digit()) {
        return apn;
    }
    let Some((base, mnc)) = rest.rsplit_once('.') else {
        return apn;
    };
    let Some(mnc_digits) = mnc
        .get(..3)
        .filter(|p| p.eq_ignore_ascii_case("mnc"))
        .and_then(|_| mnc.get(3..))
    else {
        return apn;
    };
    if base.is_empty() || mnc_digits.is_empty() || !mnc_digits.bytes().all(|b| b.is_ascii_digit()) {
        return apn;
    }
    base
}

/// Returns true when an APN refers to the IMS bearer (e.g. `ims` or `ims.MNC001.MCC460.GPRS`).
pub fn is_ims_apn(apn: &str) -> bool {
    strip_3gpp_apn_suffix(apn).eq_ignore_ascii_case("ims")
}

/// Collects CIDs whose `+CGDCONT` or `+CGCONTRDP` APN is an IMS APN.
pub(crate) fn ims_cids_from_raw(raw: &str) -> Vec<String> {
    let mut cids = Vec::new();
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        if matches!(key.trim(), "+CGDCONT" | "+CGCONTRDP") {
            let p = parser::fields(tail);
            if p.len() >= 3
                && is_ims_apn(&p[2])
                && !p[0].is_empty()
                && !cids.contains(&p[0])
            {
                cids.push(p[0].clone());
            }
        }
    }
    cids
}

fn cid_priority(cid: &str) -> u8 {
    match cid.parse::<u32>() {
        Ok(2) => 3,
        Ok(n) if n > 2 => 2,
        Ok(0) => 1,
        _ => 0,
    }
}

fn extract_data_ipv4_ipv6(raw: &str, ims_cids: &[String]) -> (Option<String>, Option<String>) {
    let mut best_v4: Option<(u8, String)> = None;
    let mut best_v6: Option<(u8, String)> = None;
    let mut update = |cid: &str, addr_str: &str| {
        if let Some(ip) = super::parser::parse_pdp_ip(addr_str) {
            let prio = if ims_cids.contains(&cid.to_string()) || cid == "1" {
                0
            } else {
                cid_priority(cid).max(1)
            };
            match ip {
                std::net::IpAddr::V4(v4) if best_v4.as_ref().is_none_or(|(p, _)| prio > *p) => {
                    best_v4 = Some((prio, v4.to_string()));
                }
                std::net::IpAddr::V6(v6) if best_v6.as_ref().is_none_or(|(p, _)| prio > *p) => {
                    best_v6 = Some((prio, v6.to_string()));
                }
                _ => {}
            }
        }
    };
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("+CGPADDR:") {
            let p = parser::fields(tail);
            if p.len() >= 2 {
                for addr in &p[1..] {
                    update(&p[0], addr);
                }
            }
        } else if let Some(tail) = line.strip_prefix("+CGCONTRDP:") {
            let p = parser::fields(tail);
            if p.len() > 3 && !is_ims_apn(&p[2]) {
                for addr in p.iter().skip(3).take(2) {
                    update(&p[0], addr);
                }
            }
        }
    }
    (best_v4.map(|(_, s)| s), best_v6.map(|(_, s)| s))
}

pub fn apn_for_plmn(plmn: &str) -> &'static str {
    match plmn.trim() {
        "46000" | "46002" | "46004" | "46007" | "46008" => "cmnet",
        "46001" | "46006" | "46009" => "3gnet",
        "46003" | "46005" | "46011" | "46012" | "46013" => "ctnet",
        "46015" => "cbnet",
        _ => "3gnet",
    }
}

pub fn carrier_for_plmn(plmn: &str) -> &'static str {
    match plmn.trim() {
        "46000" | "46002" | "46004" | "46007" | "46008" => "中国移动",
        "46001" | "46006" | "46009" => "中国联通",
        "46003" | "46005" | "46011" | "46012" | "46013" => "中国电信",
        "46015" => "中国广电",
        "46020" => "中国铁通",
        _ => "",
    }
}

/// Converts CESQ / GTCCINFO index to NR SS-RSRP in dBm (`1..=126` -> `-156..=-31`).
pub fn ss_rsrp_dbm(idx: i64) -> Option<i64> {
    (1..=126).contains(&idx).then_some(-156 + (idx - 1))
}

/// Converts CESQ / GTCCINFO index to NR SS-RSRQ in dB (`1..=126` -> `-43.0..=19.5`).
pub fn ss_rsrq_db(idx: i64) -> Option<f64> {
    (1..=126).contains(&idx).then_some(-43.0 + (idx - 1) as f64 * 0.5)
}

/// Converts CESQ index to NR SS-SINR in dB (`1..=127` -> `-23.0..=40.0`).
pub fn ss_sinr_db(idx: i64) -> Option<f64> {
    (1..=127).contains(&idx).then_some(-23.0 + (idx - 1) as f64 * 0.5)
}

/// Converts CESQ / GTCCINFO index to LTE RSRP in dBm (`1..=97` -> `-140..=-44`).
pub fn lte_rsrp_dbm(idx: i64) -> Option<i64> {
    (1..=97).contains(&idx).then_some(-140 + (idx - 1))
}

/// Converts CESQ / GTCCINFO index to LTE RSRQ in dB (`1..=34` -> `-19.5..=-3.0`).
pub fn lte_rsrq_db(idx: i64) -> Option<f64> {
    (1..=34).contains(&idx).then_some(-19.5 + (idx - 1) as f64 * 0.5)
}

fn fmt_db(v: f64) -> String {
    if (v.fract()).abs() < 1e-6 {
        format!("{}", v as i64)
    } else {
        format!("{v:.1}")
    }
}

/// Extracts GTCCINFO cell rows (`1,<rat>,...` for serving cell, `2,<rat>,...` for neighbour cells).
pub fn gtccinfo_rows(raw: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in parser::lines(raw) {
        let body = line.strip_prefix("+GTCCINFO:").unwrap_or(line).trim();
        if body.is_empty() || body.starts_with("LTE-NR") {
            continue;
        }
        let p = parser::fields(body);
        if p.len() >= 8 && matches!(p[0].as_str(), "1" | "2") && matches!(p[1].as_str(), "2" | "4" | "9") {
            rows.push(p);
        }
    }
    rows
}

pub fn preferred_nr_band(arfcn: u32) -> Option<u32> {
    let candidates = parser::nr_bands(arfcn);
    for pref in [78, 41, 1, 28, 79, 8, 5, 3] {
        if candidates.contains(&pref) {
            return Some(pref);
        }
    }
    candidates.first().copied()
}

fn decode_band_label(rat: &str, raw_band: &str, arfcn: u32) -> Option<String> {
    if !raw_band.is_empty() {
        if rat == "9" {
            if let Some(n) = raw_band.strip_prefix("50").and_then(|s| s.parse::<u32>().ok())
                && n > 0
            {
                return Some(format!("N{n}"));
            }
        } else if rat == "4"
            && let Ok(code) = raw_band.parse::<u32>()
        {
            let b = if code > 100 { code - 100 } else { code };
            if (1..=88).contains(&b) {
                return Some(format!("B{b}"));
            }
        }
    }
    if rat == "9" {
        preferred_nr_band(arfcn).map(|b| format!("N{b}"))
    } else if rat == "4" {
        parser::lte_band(arfcn).map(|b| format!("B{b}"))
    } else {
        None
    }
}

fn decode_bw_label(rat: &str, raw_bw: &str) -> Option<String> {
    let n: u32 = raw_bw.trim().parse().ok()?;
    if rat == "9" {
        (n >= 25 && n.is_multiple_of(5)).then(|| format!("{}MHz", n / 5))
    } else if rat == "4" {
        Some(
            match n {
                6 => "1.4MHz",
                15 => "3MHz",
                25 => "5MHz",
                50 => "10MHz",
                75 => "15MHz",
                100 => "20MHz",
                _ => return None,
            }
            .into(),
        )
    } else {
        None
    }
}

pub fn has_fm350_markers(raw: &str) -> bool {
    [
        "+CESQ:",
        "+GTCCINFO:",
        "+GTCAINFO:",
        "+GTSENRDTEMP:",
        "+GTDUALSIM",
        "+GTACT:",
        "+GTUSBMODE:",
        "+GTPKGVER:",
        "+GTAPPVER:",
        "+EMMCHLCK:",
        "+EMBIND:",
        "+CAVIMS:",
        "+E5GOPT:",
        "+ECELL:",
        "Fibocom",
        "FM350",
    ]
    .iter()
    .any(|m| raw.contains(m))
}

/// Augments `parser::dashboard` output when Fibocom FM350-GL responses are present.
pub fn augment_dashboard(d: &mut Value, raw: &str, temp_sum: &mut i64, temp_count: &mut i64, sim: &mut Option<bool>) {
    let mut cesq: Option<Vec<i64>> = None;
    let ims_cids = ims_cids_from_raw(raw);
    let mut best_apn: Option<(u8, String)> = None;
    for line in parser::lines(raw) {
        let up = line.to_ascii_uppercase();
        if up.starts_with("+CPIN: READY") {
            *sim = Some(true);
            d["sim"] = json!("已激活");
        }
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let p = parser::fields(tail);
        match key.trim() {
            "+GTSENRDTEMP" if p.len() >= 2 => {
                if let Ok(mdeg) = p[1].parse::<i64>()
                    && (1000..=150_000).contains(&mdeg)
                {
                    *temp_sum += (mdeg + 500) / 1000;
                    *temp_count += 1;
                }
            }
            "+GTDUALSIM" if !p.is_empty() => {
                let slot = if p[0].trim() == "1" { "2" } else { "1" };
                d["active_sim"] = json!(slot);
            }
            "+COPS" if p.len() >= 3 => {
                let op = p[2].trim();
                if !op.is_empty() {
                    if op.bytes().all(|b| b.is_ascii_digit()) {
                        d["mccmnc"] = json!(op);
                        let cname = carrier_for_plmn(op);
                        d["network_provider"] = json!(if cname.is_empty() { op } else { cname });
                    } else {
                        d["network_provider"] = json!(op);
                    }
                }
                if let Some(act) = p.get(3) {
                    let mode = match act.as_str() {
                        "11" | "12" => "NR5G-SA",
                        "13" => "NR5G-NSA",
                        "7" => "LTE",
                        "2" | "4" | "5" | "6" => "WCDMA",
                        _ => "",
                    };
                    if !mode.is_empty() && parser::text(d, "network_mode") == "-" {
                        d["network_mode"] = json!(mode);
                    }
                }
            }
            "+CESQ" if p.len() >= 9 => {
                cesq = Some(p.iter().map(|x| x.parse::<i64>().unwrap_or(255)).collect());
            }
            "+CGCONTRDP" if p.len() >= 3 => {
                let clean = strip_3gpp_apn_suffix(&p[2]);
                if !clean.is_empty() && !is_ims_apn(clean) {
                    let prio = cid_priority(&p[0]).max(1);
                    if best_apn.as_ref().is_none_or(|(p, _)| prio >= *p) {
                        best_apn = Some((prio, clean.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    if has_fm350_markers(raw) || !ims_cids.is_empty() || raw.contains("+CGPADDR:") {
        let (v4, v6) = extract_data_ipv4_ipv6(raw, &ims_cids);
        if let Some(ip) = v4 {
            d["ipv4"] = json!(ip);
            d["sim"] = json!("已激活");
        }
        if let Some(ip) = v6 {
            d["ipv6"] = json!(ip);
            d["sim"] = json!("已激活");
        }
        if let Some((_, apn)) = best_apn {
            d["apn"] = json!(apn);
        } else if is_ims_apn(parser::text(d, "apn")) {
            d["apn"] = json!("-");
        } else if parser::text(d, "apn") != "-" {
            let cur = parser::text(d, "apn").to_owned();
            let cleaned = strip_3gpp_apn_suffix(&cur);
            if cleaned != cur {
                d["apn"] = json!(cleaned);
            }
        }
    }

    let cells = gtccinfo_rows(raw);
    if let Some(sc) = cells.iter().find(|r| r[0] == "1") {
        let rat = sc[1].as_str();
        d["network_mode"] = json!(match rat {
            "9" => "NR5G-SA",
            "4" => "LTE",
            "2" => "WCDMA",
            _ => "-",
        });
        let mcc = sc[2].trim();
        let mnc_num: Option<u32> = sc[3].trim().parse().ok();
        if !mcc.is_empty()
            && let Some(mnc) = mnc_num
            && parser::text(d, "mccmnc") == "-"
        {
            let plmn = format!("{mcc}{mnc:02}");
            let cname = carrier_for_plmn(&plmn);
            d["mccmnc"] = json!(plmn);
            if parser::text(d, "network_provider") == "-" && !cname.is_empty() {
                d["network_provider"] = json!(cname);
            }
        }
        let tac = sc[4].trim();
        if !tac.is_empty() && tac != "FFFFFFF" {
            d["tac"] = json!(format!("{} ({})", tac, i64::from_str_radix(tac, 16).unwrap_or(0)));
        }
        let cid = sc[5].trim();
        if !cid.is_empty() && cid != "00FFFFFFF" {
            if cid.len() > 2 && cid.is_ascii() {
                let (prefix, last) = cid.split_at(cid.len() - 2);
                d["eNBID"] = json!(prefix);
                d["cellID"] = json!(format!(
                    "{}({}), {}({})",
                    last,
                    i64::from_str_radix(last, 16).unwrap_or(0),
                    cid,
                    i64::from_str_radix(cid, 16).unwrap_or(0)
                ));
            } else {
                d["cellID"] = json!(cid);
            }
        }
        let arfcn_str = sc[6].trim();
        let arfcn: u32 = arfcn_str.parse().unwrap_or(0);
        if !arfcn_str.is_empty() {
            d["earfcns"] = json!(arfcn_str);
        }
        let pci = sc[7].trim();
        if !pci.is_empty() {
            d["pcc_pci"] = json!(pci);
        }
        let raw_band = sc.get(8).map(String::as_str).unwrap_or("");
        if let Some(b) = decode_band_label(rat, raw_band, arfcn) {
            d["bands"] = json!(b);
        }
        let raw_bw = sc.get(9).map(String::as_str).unwrap_or("");
        if let Some(bw) = decode_bw_label(rat, raw_bw) {
            d["bandwidth"] = json!(bw);
        }

        if rat == "9" {
            let rsrp_idx = cesq
                .as_ref()
                .and_then(|c| c.get(7).copied())
                .filter(|&v| v != 255)
                .or_else(|| sc.get(12).and_then(|x| x.parse().ok()));
            let rsrq_idx = cesq
                .as_ref()
                .and_then(|c| c.get(6).copied())
                .filter(|&v| v != 255)
                .or_else(|| sc.get(13).and_then(|x| x.parse().ok()));
            let sinr_idx = cesq
                .as_ref()
                .and_then(|c| c.get(8).copied())
                .filter(|&v| v != 255);

            if let Some(rsrp) = rsrp_idx.and_then(ss_rsrp_dbm) {
                d["rsrpNR"] = json!(rsrp.to_string());
                d["rsrpNRPercentage"] = json!(((rsrp - (-120)) * 100 / 40).clamp(0, 100));
                d["prxqrsrp"] = json!(rsrp.to_string());
                d["rssi"] = json!(rsrp.to_string());
                if parser::text(d, "csq") == "99" || parser::text(d, "csq") == "-" {
                    d["csq"] = json!(((rsrp + 113) / 2).clamp(0, 31).to_string());
                }
            }
            if let Some(rsrq) = rsrq_idx.and_then(ss_rsrq_db) {
                d["rsrqNR"] = json!(fmt_db(rsrq));
                d["rsrqNRPercentage"] = json!((((rsrq as i64) - (-20)) * 100 / 12).clamp(0, 100));
            }
            let sinr_val = sinr_idx.and_then(ss_sinr_db).or_else(|| {
                sc.get(10)
                    .and_then(|x| x.parse::<i64>().ok())
                    .filter(|&v| v != 255 && (-50..=50).contains(&v))
                    .map(|v| v as f64)
            });
            if let Some(sinr) = sinr_val {
                d["sinrNR"] = json!(fmt_db(sinr));
                d["sinrNRPercentage"] = json!((((sinr as i64) * 100) / 20).clamp(0, 100));
            }
        } else if rat == "4" {
            let rsrp_idx = cesq
                .as_ref()
                .and_then(|c| c.get(5).copied())
                .filter(|&v| v != 255)
                .or_else(|| sc.get(12).and_then(|x| x.parse().ok()));
            let rsrq_idx = cesq
                .as_ref()
                .and_then(|c| c.get(4).copied())
                .filter(|&v| v != 255)
                .or_else(|| sc.get(13).and_then(|x| x.parse().ok()));
            if let Some(rsrp) = rsrp_idx.and_then(lte_rsrp_dbm) {
                d["rsrpLTE"] = json!(rsrp.to_string());
                d["rsrpLTEPercentage"] = json!(((rsrp - (-120)) * 100 / 40).clamp(0, 100));
                d["prxqrsrp"] = json!(rsrp.to_string());
                d["rssi"] = json!(rsrp.to_string());
                if parser::text(d, "csq") == "99" || parser::text(d, "csq") == "-" {
                    d["csq"] = json!(((rsrp + 113) / 2).clamp(0, 31).to_string());
                }
            }
            if let Some(rsrq) = rsrq_idx.and_then(lte_rsrq_db) {
                d["rsrqLTE"] = json!(fmt_db(rsrq));
                d["rsrqLTEPercentage"] = json!((((rsrq as i64) - (-20)) * 100 / 12).clamp(0, 100));
            }
            if let Some(sinr) = sc
                .get(10)
                .and_then(|x| x.parse::<i64>().ok())
                .filter(|&v| v != 255 && (-50..=50).contains(&v))
            {
                d["sinrLTE"] = json!(sinr.to_string());
                d["sinrLTEPercentage"] = json!((sinr * 100 / 20).clamp(0, 100));
            }
        }
    }

    // Parse +GTCAINFO: PCC/SCC lines if CA is active
    let mut ca_bands = Vec::new();
    let mut scc_pcis = Vec::new();
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("PCC:") {
            let p = parser::fields(tail);
            if p.len() >= 4 {
                let bcode = &p[0];
                let rat = if bcode.starts_with("50") { "9" } else { "4" };
                let arfcn: u32 = p[2].parse().unwrap_or(0);
                if let Some(b) = decode_band_label(rat, bcode, arfcn) {
                    ca_bands.push(b);
                }
                if parser::text(d, "bandwidth") == "-"
                    && let Some(bw) = decode_bw_label(rat, &p[3])
                {
                    d["bandwidth"] = json!(bw);
                }
            }
        } else if let Some((tag, tail)) = line.split_once(':')
            && tag.trim().starts_with("SCC")
        {
            let p = parser::fields(tail);
            if p.len() >= 6 {
                let bcode = &p[2];
                let rat = if bcode.starts_with("50") { "9" } else { "4" };
                let arfcn: u32 = p[4].parse().unwrap_or(0);
                if let Some(b) = decode_band_label(rat, bcode, arfcn)
                    && !ca_bands.contains(&b)
                {
                    ca_bands.push(b);
                }
                if !p[3].trim().is_empty() {
                    scc_pcis.push(p[3].trim().to_string());
                }
            }
        }
    }
    if !ca_bands.is_empty() {
        d["bands"] = json!(ca_bands.join(" / "));
    }
    if !scc_pcis.is_empty() {
        d["scc_pci"] = json!(scc_pcis.join(", "));
    }

    // Populate traffic counters from host RNDIS interface if +QGDNRCNT was not present
    if !raw.contains("+QGDNRCNT:")
        && has_fm350_markers(raw)
        && let Some((tx, rx)) = rndis_traffic_bytes()
    {
        d["nr_tx_bytes"] = json!(tx);
        d["nr_tx_human"] = json!(parser::human_bytes(tx as f64));
        d["nr_rx_bytes"] = json!(rx);
        d["nr_rx_human"] = json!(parser::human_bytes(rx as f64));
    }
}

pub fn augment_device(d: &mut Value, raw: &str) {
    if !has_fm350_markers(raw) && !raw.contains("+CGPADDR:") {
        return;
    }
    if parser::text(d, "manufacturer").contains("Fibocom") && parser::text(d, "modelName") == "-" {
        d["modelName"] = json!("FM350-GL");
    }
    let ims_cids = ims_cids_from_raw(raw);
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let p = parser::fields(tail);
        match key.trim() {
            "+GTPKGVER" if !p.is_empty() && !p[0].is_empty() => {
                d["firmwareVersion"] = json!(p[0]);
                if parser::text(d, "modelName") == "-" {
                    d["modelName"] = json!("FM350-GL");
                }
            }
            "+CCID" if !tail.trim().is_empty() && parser::text(d, "iccid") == "-" => {
                d["iccid"] = json!(tail.trim());
            }
            _ => {}
        }
    }
    let (v4, v6) = extract_data_ipv4_ipv6(raw, &ims_cids);
    if let Some(ip) = v4 {
        d["wwanIpv4"] = json!(ip);
    }
    if let Some(ip) = v6 {
        d["wwanIpv6"] = json!(ip);
    }
}

pub fn augment_bands(out: &mut Value, raw: &str) {
    for line in parser::lines(raw) {
        let Some(tail) = line.strip_prefix("+GTACT:") else {
            continue;
        };
        if tail.contains('(') {
            continue;
        }
        let p = parser::fields(tail);
        if p.len() < 4 {
            continue;
        }
        let mut lte = Vec::new();
        let mut nr = Vec::new();
        for token in p.iter().skip(3) {
            if let Ok(code) = token.parse::<u32>() {
                if (101..=199).contains(&code) {
                    lte.push((code - 100).to_string());
                } else if code >= 501
                    && let Some(n) = token.strip_prefix("50")
                {
                    nr.push(n.to_string());
                }
            }
        }
        if !lte.is_empty() {
            out["locked_lte_bands"] = json!(lte.join(":"));
        }
        if !nr.is_empty() {
            let joined = nr.join(":");
            out["locked_sa_bands"] = json!(joined);
            out["locked_nsa_bands"] = json!(joined);
        }
    }
}

pub fn augment_network(out: &mut Value, raw: &str) {
    let mut contr_apns: Vec<(String, String)> = Vec::new();
    let mut dcont_entries: Vec<(String, String, String)> = Vec::new();
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let p = parser::fields(tail);
        match key.trim() {
            "+GTDUALSIM" if !p.is_empty() => {
                out["sim"] = json!(if p[0].trim() == "1" { "2" } else { "1" });
            }
            "+GTACT" if !tail.contains('(') && !p.is_empty() => {
                let (pref, fallback_dis) = match p[0].as_str() {
                    "2" => ("LTE", "0"),
                    "14" => ("NR5G", "2"),
                    "17" => ("LTE:NR5G", "0"),
                    _ => ("AUTO", "0"),
                };
                out["prefNetwork"] = json!(pref);
                if parser::text(out, "nrModeControlNum") == "-" || !raw.contains("+E5GOPT:") {
                    out["nrModeControlNum"] = json!(fallback_dis);
                    out["nrModeControl"] = json!(match fallback_dis {
                        "1" => "禁用SA",
                        "2" => "禁用NSA",
                        _ => "未禁用",
                    });
                }
            }
            "+E5GOPT" if !p.is_empty() => {
                if let Ok(opt) = p[0].trim().parse::<u32>() {
                    let nr_dis = if (opt & 2) == 0 && (opt & 4) != 0 {
                        "1"
                    } else if (opt & 4) == 0 && (opt & 2) != 0 {
                        "2"
                    } else {
                        "0"
                    };
                    out["nrModeControlNum"] = json!(nr_dis);
                    out["nrModeControl"] = json!(match nr_dis {
                        "1" => "禁用SA",
                        "2" => "禁用NSA",
                        _ => "未禁用",
                    });
                }
            }
            "+CGCONTRDP" if p.len() >= 3 => {
                let clean = strip_3gpp_apn_suffix(&p[2]);
                if !clean.is_empty() && !is_ims_apn(clean) {
                    contr_apns.push((p[0].clone(), clean.to_string()));
                }
            }
            "+CGDCONT" if p.len() >= 3 && !is_ims_apn(&p[2]) => {
                dcont_entries.push((p[0].clone(), p[1].clone(), p[2].trim().to_string()));
            }
            _ => {}
        }
    }
    if has_fm350_markers(raw) {
        let best_contr = contr_apns
            .iter()
            .max_by_key(|(cid, _)| cid_priority(cid))
            .cloned();
        let best_dcont = dcont_entries
            .iter()
            .max_by_key(|(cid, _, apn)| {
                let active_bonus = u8::from(contr_apns.iter().any(|(c, _)| c == cid)) * 10;
                let nonempty_bonus = u8::from(!apn.is_empty()) * 4;
                active_bonus + nonempty_bonus + cid_priority(cid)
            })
            .cloned();
        if let Some((cid, pdp_type, apn)) = best_dcont {
            out["pdpType"] = json!(pdp_type);
            let display_apn = if !apn.is_empty() {
                strip_3gpp_apn_suffix(&apn).to_string()
            } else {
                contr_apns
                    .iter()
                    .find(|(c, _)| c == &cid)
                    .or(best_contr.as_ref())
                    .map(|(_, a)| a.clone())
                    .unwrap_or_default()
            };
            out["apn"] = json!(if display_apn.is_empty() { "-" } else { &display_apn });
        } else if let Some((_, apn)) = best_contr {
            out["apn"] = json!(apn);
        } else if is_ims_apn(parser::text(out, "apn")) {
            out["apn"] = json!("-");
        }
    }
    let cells = gtccinfo_rows(raw);
    if let Some(sc) = cells.iter().find(|r| r[0] == "1") {
        let rat = sc[1].as_str();
        let arfcn: u32 = sc[6].parse().unwrap_or(0);
        let raw_band = sc.get(8).map(String::as_str).unwrap_or("");
        if let Some(b) = decode_band_label(rat, raw_band, arfcn)
            && parser::text(out, "bands") == "-"
        {
            out["bands"] = json!(b);
        }
    }
}

pub fn augment_settings(out: &mut Value, raw: &str) {
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let p = parser::fields(tail);
        match key.trim() {
            "+GTUSBMODE" if !p.is_empty() => {
                out["currentUsbNetMode"] = json!(match p[0].as_str() {
                    "24" => "ECM",
                    "40" | "41" => "RNDIS",
                    _ => "RNDIS",
                });
            }
            "+EMBIND" if p.len() >= 2 && p[0] == "1" => {
                out["ipPassStatus"] = json!(true);
            }
            "+GTDNS" if p.len() >= 2 => {
                for dns in &p[1..] {
                    if let Some(ip) = super::parser::parse_pdp_ip(dns) {
                        if ip.is_ipv4() {
                            out["DNSV4ProxyStatus"] = json!(true);
                        } else {
                            out["DNSV6ProxyStatus"] = json!(true);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn augment_pdp(out: &mut Value, raw: &str) {
    let mut contr_apns: std::collections::HashMap<i64, String> =
        std::collections::HashMap::new();
    let mut contr_ips: std::collections::HashMap<i64, Vec<std::net::IpAddr>> =
        std::collections::HashMap::new();
    for line in parser::lines(raw) {
        let Some((key, tail)) = line.split_once(':') else {
            continue;
        };
        let p = parser::fields(tail);
        match key.trim() {
            "+GTDUALSIM" if !p.is_empty() => {
                out["sim_slot"] = json!(if p[0].trim() == "1" { 2 } else { 1 });
            }
            "+CAVIMS" if !p.is_empty() => {
                let on = p[0].trim() == "1";
                out["ims"] = json!(if on { 1 } else { 0 });
                out["volte"] = json!(on);
            }
            "+CGCONTRDP" if p.len() >= 3 => {
                let cid = parser::number(&p[0]);
                let apn = strip_3gpp_apn_suffix(&p[2]);
                if !apn.is_empty() {
                    contr_apns.entry(cid).or_insert_with(|| apn.to_string());
                }
                for addr in p.iter().skip(3).take(2) {
                    if let Some(ip) = super::parser::parse_pdp_ip(addr) {
                        contr_ips.entry(cid).or_default().push(ip);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(contexts) = out["contexts"].as_array_mut() {
        let has_context_info =
            raw.contains("+CGACT:") || raw.contains("+CGCONTRDP:") || has_fm350_markers(raw);
        for ctx in contexts {
            let cid = ctx["cid"].as_i64().unwrap_or(0);
            if let Some(apn) = contr_apns.get(&cid) {
                ctx["apn"] = json!(apn);
            }
            if let Some(ips) = contr_ips.get(&cid) {
                if ctx["ipv4"].is_null()
                    && let Some(v4) = ips.iter().find(|ip| ip.is_ipv4())
                {
                    ctx["ipv4"] = json!(v4.to_string());
                }
                if ctx["ipv6"].is_null()
                    && let Some(v6) = ips.iter().find(|ip| ip.is_ipv6())
                {
                    ctx["ipv6"] = json!(v6.to_string());
                }
            }
            if ctx["active"].is_null() && has_context_info {
                let has_ip = !ctx["ipv4"].is_null() || !ctx["ipv6"].is_null();
                ctx["active"] = json!(has_ip);
            }
        }
    }
}

pub fn augment_cell_locks(out: &mut Value, raw: &str) {
    for line in parser::lines(raw) {
        let Some(tail) = line.strip_prefix("+EMMCHLCK:") else {
            continue;
        };
        if tail.contains('(') {
            continue;
        }
        let p = parser::fields(tail);
        if p.len() >= 4 && p[0] == "1" {
            let act = p[1].as_str();
            let earfcn: u64 = p[3].parse().unwrap_or(0);
            // Frequency locks leave PCI empty (`+EMMCHLCK: 1,11,1,426030,,0`), while cell locks include PCI.
            let Some(pci) = p.get(4).and_then(|s| s.trim().parse::<u64>().ok()) else {
                continue;
            };
            if act == "7" {
                out["lte"] = json!([1, earfcn, pci]);
            } else if act == "11" {
                let band = preferred_nr_band(earfcn as u32).unwrap_or(78);
                let scs = default_nr_scs(band).unwrap_or(30);
                out["nr"] = json!([pci, earfcn, scs, band]);
            }
        }
    }
}

pub fn default_nr_scs(band: u32) -> Option<u32> {
    match band {
        1 | 2 | 3 | 5 | 7 | 8 | 12 | 14 | 18 | 20 | 25 | 26 | 28 | 30 | 66 | 71 => Some(15),
        38 | 40 | 41 | 48 | 77 | 78 | 79 => Some(30),
        _ => None,
    }
}

pub fn augment_neighbours_and_scan(nr: &mut Vec<Value>, lte: &mut Vec<Value>, raw: &str, for_scan: bool) {
    let cells = gtccinfo_rows(raw);
    if cells.is_empty() {
        return;
    }
    let mut serving_carrier = String::from("当前网络");
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("+COPS:") {
            let p = parser::fields(tail);
            if let Some(op) = p.get(2) {
                let c = carrier_for_plmn(op);
                if !c.is_empty() {
                    serving_carrier = c.into();
                }
            }
        }
    }

    let mut serving_nr_freq: Option<String> = None;
    let mut serving_lte_freq: Option<String> = None;

    for row in cells {
        let is_serving = row[0] == "1";
        let rat = row[1].as_str();
        if is_serving {
            let mcc = row[2].trim();
            if let Ok(mnc) = row[3].trim().parse::<u32>() {
                let c = carrier_for_plmn(&format!("{mcc}{mnc:02}"));
                if !c.is_empty() {
                    serving_carrier = c.into();
                }
            }
        }
        let freq = row[6].trim().to_string();
        let pci = row[7].trim().to_string();
        if freq.is_empty() || pci.parse::<u32>().is_err() {
            continue;
        }
        let arfcn: u32 = freq.parse().unwrap_or(0);
        if rat == "9" {
            if is_serving {
                serving_nr_freq = Some(freq.clone());
            }
            let rsrp_idx: i64 = if is_serving {
                row.get(12).and_then(|s| s.parse().ok()).unwrap_or(255)
            } else {
                row.get(9).and_then(|s| s.parse().ok()).unwrap_or(255)
            };
            let rsrp = ss_rsrp_dbm(rsrp_idx)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into());
            let band_num = row
                .get(8)
                .and_then(|s| s.strip_prefix("50"))
                .and_then(|s| s.parse::<u32>().ok())
                .or_else(|| preferred_nr_band(arfcn));
            let band_str = band_num
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into());
            let scs = band_num
                .and_then(default_nr_scs)
                .map(|v| json!(v))
                .unwrap_or(Value::Null);
            if nr.iter().any(|c| c["freq"] == freq && c["pci"] == pci) {
                continue;
            }
            if for_scan {
                nr.push(json!({
                    "type": "NR5G",
                    "provider": serving_carrier,
                    "band": band_str,
                    "freq": freq,
                    "pci": pci,
                    "rsrp": rsrp,
                    "scs": scs,
                }));
            } else {
                let role = if is_serving {
                    "服务小区"
                } else if serving_nr_freq.as_deref() == Some(freq.as_str()) {
                    "同频邻区"
                } else {
                    "异频邻区"
                };
                nr.push(json!({
                    "type": "NR5G",
                    "role": role,
                    "band": band_str,
                    "freq": freq,
                    "pci": pci,
                    "rsrp": rsrp,
                    "scs": scs,
                }));
            }
        } else if rat == "4" {
            if is_serving {
                serving_lte_freq = Some(freq.clone());
            }
            let rsrp_idx: i64 = if is_serving {
                row.get(12).and_then(|s| s.parse().ok()).unwrap_or(255)
            } else {
                row.get(9).and_then(|s| s.parse().ok()).unwrap_or(255)
            };
            let rsrp = lte_rsrp_dbm(rsrp_idx)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into());
            let band_num = row
                .get(8)
                .and_then(|s| s.parse::<u32>().ok())
                .map(|c| if c > 100 { c - 100 } else { c })
                .or_else(|| parser::lte_band(arfcn));
            let band_str = band_num
                .map(|b| b.to_string())
                .unwrap_or_else(|| "-".into());
            if lte.iter().any(|c| c["freq"] == freq && c["pci"] == pci) {
                continue;
            }
            if for_scan {
                lte.push(json!({
                    "type": "LTE",
                    "provider": serving_carrier,
                    "band": band_str,
                    "freq": freq,
                    "pci": pci,
                    "rsrp": rsrp,
                }));
            } else {
                let role = if is_serving {
                    "服务小区"
                } else if serving_lte_freq.as_deref() == Some(freq.as_str()) {
                    "同频邻区"
                } else {
                    "异频邻区"
                };
                lte.push(json!({
                    "type": "LTE",
                    "role": role,
                    "band": band_str,
                    "freq": freq,
                    "pci": pci,
                    "rsrp": rsrp,
                }));
            }
        }
    }

    // Parse any extra NR/LTE cells from AT+ECELL (each cell record is 15 comma-separated fields)
    for line in parser::lines(raw) {
        let Some(tail) = line.strip_prefix("+ECELL:") else {
            continue;
        };
        let p = parser::fields(tail);
        if p.len() < 16 {
            continue;
        }
        let mut idx = 1usize;
        while idx + 15 <= p.len() {
            let chunk = &p[idx..idx + 15];
            idx += 15;
            let act = chunk[0].as_str();
            let pci = chunk[5].trim();
            let rsrp_idx: i64 = chunk[6].parse().unwrap_or(255);
            let freq = chunk[14].trim();
            if freq.is_empty() || pci.parse::<u32>().is_err() {
                continue;
            }
            let Ok(arfcn) = freq.parse::<u32>() else {
                continue;
            };
            if act == "11" {
                if nr.iter().any(|c| c["freq"] == freq && c["pci"] == pci) {
                    continue;
                }
                let rsrp = ss_rsrp_dbm(rsrp_idx)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                let band_num = preferred_nr_band(arfcn);
                let band_str = band_num
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "-".into());
                let scs = band_num
                    .and_then(default_nr_scs)
                    .map(|v| json!(v))
                    .unwrap_or(Value::Null);
                if for_scan {
                    nr.push(json!({
                        "type": "NR5G",
                        "provider": serving_carrier,
                        "band": band_str,
                        "freq": freq,
                        "pci": pci,
                        "rsrp": rsrp,
                        "scs": scs,
                    }));
                } else {
                    let role = if serving_nr_freq.as_deref() == Some(freq) {
                        "同频邻区"
                    } else {
                        "异频邻区"
                    };
                    nr.push(json!({
                        "type": "NR5G",
                        "role": role,
                        "band": band_str,
                        "freq": freq,
                        "pci": pci,
                        "rsrp": rsrp,
                        "scs": scs,
                    }));
                }
            } else if act == "7" {
                if lte.iter().any(|c| c["freq"] == freq && c["pci"] == pci) {
                    continue;
                }
                let rsrp = lte_rsrp_dbm(rsrp_idx)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                let band_str = parser::lte_band(arfcn)
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "-".into());
                if for_scan {
                    lte.push(json!({
                        "type": "LTE",
                        "provider": serving_carrier,
                        "band": band_str,
                        "freq": freq,
                        "pci": pci,
                        "rsrp": rsrp,
                    }));
                } else {
                    let role = if serving_lte_freq.as_deref() == Some(freq) {
                        "同频邻区"
                    } else {
                        "异频邻区"
                    };
                    lte.push(json!({
                        "type": "LTE",
                        "role": role,
                        "band": band_str,
                        "freq": freq,
                        "pci": pci,
                        "rsrp": rsrp,
                    }));
                }
            }
        }
    }
}

pub fn augment_mbn(out: &mut Value, raw: &str) {
    for line in parser::lines(raw) {
        if let Some(tail) = line.strip_prefix("+GTACT:")
            && tail.contains('(')
        {
            // +GTACT: (1,2,4,10,14,16,17,20),(2,3,6),(2,3,6),(),(1,2,4,5,8),(101,...),(),(),(501,...)
            let groups: Vec<&str> = tail
                .split('(')
                .skip(1)
                .filter_map(|s| s.split(')').next())
                .collect();
            if let Some(lte_grp) = groups.get(5) {
                let lte: Vec<String> = lte_grp
                    .split(',')
                    .filter_map(|s| s.trim().parse::<u32>().ok())
                    .filter(|&c| (101..=199).contains(&c))
                    .map(|c| (c - 100).to_string())
                    .collect();
                if !lte.is_empty() {
                    out["policy_lte_bands"] = json!(lte.join(":"));
                }
            }
            if let Some(nr_grp) = groups.get(8) {
                let nr: Vec<String> = nr_grp
                    .split(',')
                    .filter_map(|s| s.trim().strip_prefix("50"))
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                if !nr.is_empty() {
                    let s = nr.join(":");
                    out["policy_sa_bands"] = json!(s);
                    out["policy_nsa_bands"] = json!(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm350_dashboard_parses_live_f32_output() {
        let raw = "\
+CPIN: READY\r\n\
+CESQ: 99,99,255,255,255,255,65,53,63\r\n\
+GTSENRDTEMP: 1,39370\r\n\
+GTDUALSIM : 0, \"SUB1\", \"NR\"\r\n\
+COPS:0,2,\"46001\",11\r\n\
+CEREG: 0,1\r\n\
+GTCCINFO: \r\n\
1,9,460,1,5CE100,5CE05A427,426030,835,,,8,52,52,64\r\n\
2,9,,,FFFFFFF,00FFFFFFF,633984,433,,45,45,63\r\n\
+CGCONTRDP: 1,,\"ims\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+CGCONTRDP: 2,,\"3gnet\",\"\",\"\",\"112.65.184.255\",\"210.22.84.3\"\r\n\
+CGPADDR: 2,\"10.38.19.168\",\"0.0.0.0.0.0.0.0.24.232.60.28.160.253.122.226\"\r\n\
OK\r\n";
        let d = parser::dashboard(raw);
        assert_eq!(d["sim"], "已激活");
        assert_eq!(d["active_sim"], "1");
        assert_eq!(d["temperature"], "39");
        assert_eq!(d["network_provider"], "中国联通");
        assert_eq!(d["mccmnc"], "46001");
        assert_eq!(d["network_mode"], "NR5G-SA");
        assert_eq!(d["apn"], "3gnet");
        assert_eq!(d["ipv4"], "10.38.19.168");
        assert_eq!(d["bands"], "N1");
        assert_eq!(d["earfcns"], "426030");
        assert_eq!(d["pcc_pci"], "835");
        assert_eq!(d["rsrpNR"], "-104");
        assert_eq!(d["rsrqNR"], "-11");
        assert_eq!(d["sinrNR"], "8");
    }

    #[test]
    fn fm350_bands_and_gtact_builder_roundtrip() {
        let raw = "+GTACT: 20,6,3,1,2,4,5,8,101,103,105,108,141,501,5028,5041,5078,5079\r\nOK\r\n";
        let b = parser::bands(raw);
        assert_eq!(b["locked_lte_bands"], "1:3:5:8:41");
        assert_eq!(b["locked_sa_bands"], "1:28:41:78:79");

        let cmd = build_gtact_command_with_caps(raw, "", None, None, Some(&[1, 3, 8]), Some(&[1, 78]));
        assert_eq!(
            cmd,
            "AT+GTACT=20,6,3,1,2,4,5,8,101,103,108,501,5078"
        );
    }

    #[test]
    fn fm350_neighbours_and_scan_from_gtccinfo() {
        let raw = "\
+COPS:0,2,\"46001\",11\r\n\
+GTCCINFO: \r\n\
1,9,460,1,5CE100,5CE05A427,426030,835,,,8,52,52,64\r\n\
2,9,,,FFFFFFF,00FFFFFFF,426030,212,,60,60,39\r\n\
2,9,,,FFFFFFF,00FFFFFFF,633984,433,,45,45,63\r\n\
OK\r\n";
        let nb = parser::neighbours(raw);
        let nr = nb["nr5g_cells_parsed"].as_array().unwrap();
        assert_eq!(nr.len(), 3);
        assert_eq!(nr[0]["role"], "服务小区");
        assert_eq!(nr[0]["band"], "1");
        assert_eq!(nr[0]["scs"], 15);
        assert_eq!(nr[1]["role"], "同频邻区");
        assert_eq!(nr[2]["role"], "异频邻区");
        assert_eq!(nr[2]["band"], "78");
        assert_eq!(nr[2]["scs"], 30);

        let sc = parser::scan(raw);
        let snr = sc["nr5g_cells_parsed"].as_array().unwrap();
        assert_eq!(snr.len(), 3);
        assert_eq!(snr[0]["provider"], "中国联通");
    }

    #[test]
    fn fm350_emmchlck_distinguishes_cell_lock_and_freq_lock() {
        let cell_raw = "+EMMCHLCK: 1,11,1,426030,835,0\r\nOK\r\n";
        let locks = parser::cell_locks(cell_raw);
        assert_eq!(locks["nr"], json!([835, 426030, 15, 1]));
        assert_eq!(parser::nr_freq_lock(cell_raw), Some(vec![]));

        let freq_raw = "+EMMCHLCK: 1,11,1,426030,,0\r\nOK\r\n";
        let locks2 = parser::cell_locks(freq_raw);
        assert!(locks2["nr"].is_null());
        assert_eq!(parser::nr_freq_lock(freq_raw), Some(vec![(426030, 15)]));
    }

    #[test]
    fn fm350_run_translated_skips_inactive_cids_without_aborting() {
        let mut seen = Vec::new();
        let res = run_translated("AT+CGDCONT?;+CGACT?;+CGPADDR", |cmd| {
            seen.push(cmd.to_string());
            if cmd == "AT+CGDCONT?;+CGACT?;+CGCONTRDP" {
                Ok("+CGDCONT: 1,\"IPV4V6\",\"3gnet\"\r\n+CGCONTRDP: 1,,\"ims\",\"\"\r\nOK\r\n".into())
            } else if cmd == "AT+CGPADDR=1" {
                Ok("+CGPADDR: 1,\"0.0.0.0.0.0.0.0.24.232.60.130.87.214.61.172\",\"\"\r\nOK\r\n".into())
            } else {
                Ok("ERROR\r\n".into())
            }
        })
        .unwrap()
        .unwrap();
        assert_eq!(seen, vec!["AT+CGDCONT?;+CGACT?;+CGCONTRDP", "AT+CGPADDR=1"]);
        assert!(res.contains("+CGPADDR: 1,"));
    }

    #[test]
    fn fm350_ecell_adds_extra_nr_cells() {
        let raw = "\
+COPS:0,2,\"46001\",11\r\n\
+GTCCINFO: \r\n\
1,9,460,1,5CE100,5CE05A427,426030,835,,,8,52,52,64\r\n\
+ECELL: 2,11,\"5CE05A427\",\"5CE100\",460,1,835,52,63,255,255,2,63,255,,426030,11,\"00FFFFFFF\",\"FFFFFFF\",,,433,44,62,255,255,,56,255,,633984\r\n\
OK\r\n";
        let sc = parser::scan(raw);
        let snr = sc["nr5g_cells_parsed"].as_array().unwrap();
        assert_eq!(snr.len(), 2);
        assert_eq!(snr[1]["freq"], "633984");
        assert_eq!(snr[1]["band"], "78");
        assert_eq!(snr[1]["pci"], "433");
    }

    #[test]
    fn fm350_pdp_prefers_dynamic_cgcontrdp_apn() {
        let raw = "\
+CGDCONT: 2,\"IPV4V6\",\"3gnet\",\"\",,,,,,,,,,\r\n\
+CGDCONT: 1,\"IPV4V6\",\"3gnet\",\"\",,,,,,,,,,\r\n\
+CGACT: 2,1\r\n\
+CGCONTRDP: 1,,\"ims\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+CGCONTRDP: 2,,\"3gnet\",\"\",\"\",\"112.65.184.255\"\r\n\
+CGPADDR: 1,\"0.0.0.0.0.0.0.0.24.232.60.130.87.214.61.172\",\"\"\r\n\
+CGPADDR: 2,\"10.227.14.22\",\"0.0.0.0.0.0.0.0.24.232.63.118.97.4.201.155\"\r\n\
OK\r\n";
        let d = parser::pdp(raw);
        let c = d["contexts"].as_array().unwrap();
        assert_eq!(c[0]["cid"], 2);
        assert_eq!(c[0]["apn"], "3gnet");
        assert_eq!(c[1]["cid"], 1);
        assert_eq!(c[1]["apn"], "ims");
        assert_eq!(c[1]["active"], true);
    }

    #[test]
    fn fm350_gnss_nmea_parsers_and_chrony_packet() {
        let rmc = "$GNRMC,165106.125,A,3117.8021,N,12130.3690,E,0.000,0.00,151126,,,A,V*3D";
        let utc = parse_rmc_utc(rmc).expect("valid rmc utc");
        assert!((utc - 1_794_761_466.125).abs() < 1e-6);

        let pkt = build_chrony_sock_packet(1_794_761_466, 228_000, -0.103);
        assert_eq!(&pkt[36..40], &0x534f_434b_i32.to_ne_bytes());
        assert_eq!(i64::from_ne_bytes(pkt[0..8].try_into().unwrap()), 1_794_761_466);
        assert_eq!(i64::from_ne_bytes(pkt[8..16].try_into().unwrap()), 228_000);
        assert!((f64::from_ne_bytes(pkt[16..24].try_into().unwrap()) + 0.103).abs() < 1e-9);
    }

    #[test]
    fn fm350_lte_apn_ignores_ims_and_strips_3gpp_suffix() {
        assert_eq!(strip_3gpp_apn_suffix("3gnet.MNC001.MCC460.GPRS"), "3gnet");
        assert_eq!(strip_3gpp_apn_suffix("ims.MNC001.MCC460.GPRS"), "ims");
        assert_eq!(strip_3gpp_apn_suffix("cmnet.sh.mnc000.mcc460.gprs"), "cmnet.sh");
        assert_eq!(strip_3gpp_apn_suffix("custom.corp.apn"), "custom.corp.apn");
        assert!(is_ims_apn("ims.MNC001.MCC460.GPRS"));
        assert!(is_ims_apn("IMS"));
        assert!(!is_ims_apn("3gnet.MNC001.MCC460.GPRS"));

        let raw = "\
+CPIN: READY\r\n\
+CESQ: 17,99,255,255,20,37,255,255,255\r\n\
+GTSENRDTEMP: 1,39431\r\n\
+GTDUALSIM : 0, \"SUB1\", \"NR\"\r\n\
+COPS:0,2,\"46001\",13\r\n\
+CEREG: 0,1\r\n\
+C5GREG: 0\r\n\
+GTCCINFO: \r\n\
1,4,460,1,E888,00E8CD63D,1750,433,,,5,36,36,18\r\n\
+CGCONTRDP: 0,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n\
+CGCONTRDP: 1,6,\"ims.MNC001.MCC460.GPRS\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+EMBIND: 0\r\n\
+CGPADDR: 0,\"10.106.143.127\",\"0.0.0.0.0.0.0.0.24.232.131.10.126.65.142.171\"\r\n\
+CGPADDR: 1,\"0.0.0.0.0.0.0.0.24.232.131.10.146.104.4.243\",\"\"\r\n\
OK\r\n";
        let d = parser::dashboard(raw);
        assert_eq!(d["apn"], "3gnet");
        assert_eq!(d["ipv4"], "10.106.143.127");
        assert_eq!(d["ipv6"], "::18e8:830a:7e41:8eab");
        assert_eq!(d["network_mode"], "LTE");

        let pdp_raw = "\
+CGDCONT: 0,\"IPV4V6\",\"3gnet\",\"\",0,0,0,2,0,0,,0,1,0\r\n\
+CGDCONT: 1,\"IPV4V6\",\"ims\",\"\",0,0,0,2,1,1,,0,1,0\r\n\
+CGCONTRDP: 0,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n\
+CGCONTRDP: 1,6,\"ims.MNC001.MCC460.GPRS\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+CGPADDR: 0,\"10.106.143.127\",\"0.0.0.0.0.0.0.0.24.232.131.10.126.65.142.171\"\r\n\
+CGPADDR: 1,\"0.0.0.0.0.0.0.0.24.232.131.10.146.104.4.243\",\"\"\r\n\
OK\r\n";
        let pdp = parser::pdp(pdp_raw);
        let ctxs = pdp["contexts"].as_array().unwrap();
        assert_eq!(ctxs[0]["cid"], 0);
        assert_eq!(ctxs[0]["apn"], "3gnet");
        assert_eq!(ctxs[1]["cid"], 1);
        assert_eq!(ctxs[1]["apn"], "ims");

        let net_raw = "\
+GTDUALSIM : 0, \"SUB1\", \"NR\"\r\n\
+GTACT: 2,3,3,101,103\r\n\
+E5GOPT: 7\r\n\
+CGDCONT: 2,\"IPV4V6\",\"\",\"\",,,,,,,,,,\r\n\
+CGDCONT: 1,\"IPV4V6\",\"ims\",\"\",0,0,0,2,1,1,,0,1,0\r\n\
+CGCONTRDP: 2,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n\
+CGCONTRDP: 1,6,\"ims.MNC001.MCC460.GPRS\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+EMMCHLCK: 0\r\nOK\r\n";
        let net = parser::network(net_raw);
        assert_eq!(net["prefNetwork"], "LTE");
        assert_eq!(net["pdpType"], "IPV4V6");
        assert_eq!(net["apn"], "3gnet");
    }

    #[test]
    fn fm350_ensure_data_session_ignores_cid0_and_activates_cid2_on_lte() {
        let mut raw = "\
+CPIN: READY\r\n\
+COPS:0,2,\"46001\",13\r\n\
+CEREG: 0,1\r\n\
+CGCONTRDP: 0,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n\
+CGCONTRDP: 1,6,\"ims.MNC001.MCC460.GPRS\",\"\",\"\",\"36.8.136.136.0.0.136.136.0.0.0.0.0.0.0.8\"\r\n\
+EMBIND: 1,\"M-RNDIS\",2\r\n\
+CGPADDR: 0,\"10.46.68.24\",\"0.0.0.0.0.0.0.0.24.232.131.10.126.65.142.171\"\r\n\
OK\r\n"
            .to_string();
        let throttle = Mutex::new(None);
        let mut ran = Vec::new();
        ensure_data_session_with_throttle(&mut raw, &throttle, |cmd| {
            ran.push(cmd.to_string());
            match cmd {
                "AT+CGDCONT?" => Ok("+CGDCONT: 0,\"IPV4V6\",\"3gnet\"\r\n+CGDCONT: 1,\"IPV4V6\",\"ims\"\r\nOK\r\n".into()),
                "AT+CGDCONT=2,\"IPV4V6\",\"3gnet\"" | "AT+CGACT=1,2" => Ok("\r\nOK\r\n".into()),
                "AT+CGCONTRDP=2;+CGPADDR=2" => Ok("+CGCONTRDP: 2,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n+CGPADDR: 2,\"10.46.68.24\",\"\"\r\nOK\r\n".into()),
                _ => Ok("OK\r\n".into()),
            }
        });
        assert_eq!(
            ran,
            vec![
                "AT+CGDCONT?",
                "AT+CGDCONT=2,\"IPV4V6\",\"3gnet\"",
                "AT+CGACT=1,2",
                "AT+CGCONTRDP=2;+CGPADDR=2",
            ]
        );
        assert!(raw.contains("+CGPADDR: 2,\"10.46.68.24\""));

        // Also verify missing EMBIND triggers bind even if CID 2 is already active.
        let mut raw_unbound = "\
+CPIN: READY\r\n\
+CEREG: 0,1\r\n\
+CGCONTRDP: 2,5,\"3gnet.MNC001.MCC460.GPRS\",\"\",\"\",\"112.65.184.255\"\r\n\
+EMBIND: 0\r\n\
+CGPADDR: 2,\"10.46.68.24\",\"\"\r\nOK\r\n"
            .to_string();
        let throttle2 = Mutex::new(None);
        let mut ran2 = Vec::new();
        ensure_data_session_with_throttle(&mut raw_unbound, &throttle2, |cmd| {
            ran2.push(cmd.to_string());
            Ok("\r\nOK\r\n".into())
        });
        assert_eq!(ran2, vec!["AT+EMBIND=1,\"M-RNDIS\",2"]);
    }

    #[test]
    fn fm350_save_settings_targets_cid2_and_gtact_switches_out_of_lte_only() {
        let mut seen = Vec::new();
        let res = execute_action(
            "AT+CGDCONT=1,\"IPV4V6\",\"cmnet\";+QNWPREFCFG=\"mode_pref\",AUTO",
            |cmd| {
                seen.push(cmd.to_string());
                match cmd {
                    "AT+GTACT?" => Ok("+GTACT: 2,3,3,101,103\r\nOK\r\n".into()),
                    "AT+GTACT=?" => Ok("+GTACT: (1,2,4,10,14,16,17,20),(2,3,6),(2,3,6),(),(1,8),(101,103),(),(),(501,5078)\r\nOK\r\n".into()),
                    _ => Ok("\r\nOK\r\n".into()),
                }
            },
        )
        .unwrap()
        .unwrap();
        assert!(parser::ok(&res));
        assert_eq!(
            seen,
            vec![
                "AT+CGDCONT=2,\"IPV4V6\",\"cmnet\"",
                "AT+GTACT?",
                "AT+GTACT=?",
                "AT+GTACT=20,6,3,1,8,101,103,501,5078",
            ]
        );
    }
}
