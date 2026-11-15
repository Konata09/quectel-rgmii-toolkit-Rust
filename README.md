# FM350-GL Toolkit · Rust

专为 **广和通 Fibocom FM350-GL**（实测固件 `81600.0000.00.29.24.02` / `FM350.F32`）改造的 Rust 轻量管理后台，基于 `quectel-rgmii-toolkit-Rust` 架构精简并适配，在宿主机（实测 Jetson AGX Xavier `aarch64` Ubuntu）运行 `simpleadmin-httpd`，通过 USB AT 串口、`usb0` RNDIS 网卡与 ADB `fm350-agent` 管理模块。

> 本仓库面向 FM350-GL 单线定制，**已在前端直接隐藏所有 FM350-GL 固件不支持的移远专属功能**，并在后端显式拒绝这些命令，不再保留兼容性桩代码或合并上游计划。

## 系统架构

FM350-GL 模块内部为精简 OpenWrt（约 `104 MB` 内存，只读固件分区空间有限），且宿主机通过 USB 连接模块（VID/PID `0e8d:7127`），因此采用**“宿主机主服务 + 模块内轻量探针”**架构：

```text
  WebUI (:8080 HTTP)
         │
         ▼
  simpleadmin-httpd (Linux 宿主机 / Jetson)
    ├── /dev/fm350-at (USB 接口 06) ──▶ AT 指令、信号/频段/小区锁定、短信、温度 (AT+GTSENRDTEMP=1)
    ├── usb0 (M-RNDIS, MAC 重命名)   ──▶ IPv4/IPv6 自动同步、流量统计 (/sys/class/net/usb0/statistics)
    │                                   ICMP 延迟/抖动/丢包监测 (surge_ping + SO_BINDTODEVICE)
    │                                   iptables/ip6tables TTL 修改
    ├── 127.0.0.1:19090 (ADB 转发)   ──▶ 模块内 /usr/bin/fm350-agent (读取模块 /proc/stat, meminfo, uptime)
    ├── 127.0.0.1:19091 (ADB 转发)   ──▶ 模块内 fm350-agent 连接 @mtk_gnssadp_at (FM350 GNSS 星图/定位 + /run/chrony/fm350.sock)
    └── chronyc (宿主机)             ──▶ 板载 PPS 主授时 + FM35 (FM350 SOCK) 时间热备
```

### 关键设计说明

1. **USB 串口安全边界（极其重要）**
   - FM350-GL (`0e8d:7127`) 暴露多个 USB 接口，**仅 `bInterfaceNumber == "06"`（如 `/dev/ttyUSB4` → `/dev/fm350-at`）可发送 AT 指令**。
   - `bInterfaceNumber == "05"` 为 **ADB 接口**，必须在 udev 规则中从 `option` 驱动 `unbind`，否则 `adb` 无法连接模块。
   - **禁止向其他 `ttyUSB*`（如 `00`、`02`、`03`、`04`、`07`）写入探测数据**，避免触发模块端口阻塞或 Watchdog 重启。
   - `src/at.rs` 打开串口前会执行 `std::fs::canonicalize`，确保通过 `/dev/fm350-at` 等符号链接访问时同样受全局串口互斥锁保护。
2. **`usb0` 接口与双栈网络自动恢复**
   - 通过 `systemd` `.link` 规则将 `0e8d:7127` RNDIS 网卡固定命名为 `usb0`，并设为 NetworkManager `unmanaged`。
   - `src/fm350.rs` (`ensure_data_session` / `sync_rndis_network`) 自动维护 `AT+CGDCONT=2`、`AT+CGACT=1,2` 与 `AT+EMBIND=1,"M-RNDIS",2`，并将 IPv4 地址、Metric `101` 默认路由及运营商 DNS 写入 `usb0` 与 `systemd-resolved`。
   - 针对 `M-RNDIS` 下 IPv6 路由器通告 / 邻居发现特性，自动打开 `accept_ra=1` / `autoconf=1`，主动发送 ICMPv6 Router Solicitation（RS），固定网关邻居 `fe80::1 lladdr 00:00:88:ff:00:00 router nud permanent`，并配置 IPv6 默认路由（Metric `101`）。
3. **纯模块侧监控口径**
   - **CPU / 内存 / 在线时长**：由运行在 FM350-GL 模块内的静态 Rust 程序 `scripts/fm350_agent.rs`（`aarch64-unknown-linux-musl`，约 `423 KB`）提供，宿主机通过 `adb forward tcp:19090 tcp:19090` 拉取模块自身 `/proc/stat`、`/proc/meminfo` 和 `/proc/uptime`。
   - **温度**：读取 `AT+GTSENRDTEMP=1`。
   - **下载 / 上传流量与速率**：由于 FM350-GL `M-RNDIS` 数据路径绕过模块内 `ccmni0` 计数器，流量直接采集宿主机 `usb0` 的 `rx_bytes` / `tx_bytes`。
   - **延迟 / 抖动 / 丢包**：`surge_ping` 绑定 `usb0` 接口（`SO_BINDTODEVICE`）发包，仅反映模块蜂窝链路质量。
4. **FM350-GL GNSS 卫星页面与时间热备（ADB `@mtk_gnssadp_at` + `chrony SOCK`）**
   - FM350-GL 的 GNSS AT 控制面运行在模块 AP 侧 OpenWrt 的 `@mtk_gnssadp_at`（不走 Modem `06` 口），且无硬件 1PPS 引脚。
   - `fm350-agent` 通过 `@mtk_gnssadp_at` 启用全星座定位（`AT+GTGPSSWITCH=1`、`AT+GTGPSCFG=2,6`、`AT+GTGPSEPO=1`、`AT+ELCSNMEACFG=1,63`、`AT+GTGPSPOWER=1`），并在 `127.0.0.1:19091` 持续向宿主机推流标准 NMEA-0183 报文（经 `adb forward tcp:19091 tcp:19091`）。
   - `simpleadmin-httpd` 后台接收该 NMEA 流后，一方面直接供给 WebUI **GNSS 授时**页面的卫星星图、坐标与 SNR 历史曲线（与宿主机 `gpsd` / 板载 ATGM332D 相互独立）；另一方面解析有效 `$GNRMC` 时间并向 `/run/chrony/fm350.sock` 推送 `chrony` `SOCK` 采样（`refid FM35`、`offset 0.103`、`delay 0.5`），在不影响板载 `PPS`（`Stratum 1`）的前提下保持离线时间热备可达。

## 功能支持清单

### 已适配并可用

| 页面 | 功能 | 模块 AT / 系统实现 |
| --- | --- | --- |
| 总览 | 信号、频段、小区详情、PRX RSRP 天线 | `AT+GTCCINFO?`、`AT+GTCAINFO?`、`AT+CSQ`、`AT+COPS?` |
| 总览 | 模块 CPU、内存、在线时长、温度、流量、ICMP 延迟走势 | `fm350-agent` (ADB `:19090`)、`AT+GTSENRDTEMP=1`、`usb0` |
| 网络事件 / 断网恢复 | 制式/小区变更事件、四级看门狗（重注册/射频/切卡/重启） | `AT+COPS=2/0`、`AT+CFUN=0/1`、`AT+GTDUALSIM`、`AT+CFUN=1,1` |
| GNSS 授时 | FM350 卫星星图/SNR 与 chrony 时间源（主 PPS + 备 FM35） | `chronyc`、`fm350-agent` (ADB `:19091`) + `/run/chrony/fm350.sock` |
| 频段与频点 | 制式切换（AUTO/LTE/NSA/SA）、频段锁定、单 NR5G 频点锁定 | `AT+GTACT`、`AT+E5GOPT`、`AT+EMMCHLCK=1,11,0,<arfcn>`（30s 自动保护回退） |
| 小区锁定 | 邻区与小区扫描、单小区锁定（LTE 或 NR5G-SA） | `AT+GTCCINFO?`、`AT+ECELL`、`AT+EMMCHLCK`（LTE `1,7,0,<earfcn>,<pci>` / NR `1,11,0,<arfcn>,<pci>`） |
| 拨号与 SIM | APN/PDP 增删改与激活、IMS 开关、双卡槽切换 | `AT+CGDCONT`、`AT+CGACT`、`AT+CAVIMS=0/1`、`AT+GTDUALSIM=0/1` |
| 短信 / 消息通知 | SIM 卡短信收发与清理、各渠道转发与断网通知 | `AT+CPMS="SM"`、`AT+CMGF=0` PDU 模式 |
| 系统设置 | IMEI 修改、TTL 修改、AT 终端与队列、Web 账号/端口/语言、重启/恢复出厂 | `AT+EGMREXT=1,7,"<imei>"`、`iptables`/`ip6tables`、`AT+CFUN=1,1`、`AT&F` |
| 设备信息 / 控制台 | 模块型号/固件/SIM/WAN IP 展示、宿主机 root 网页终端 | `ATI`/`AT+CGMM`/`AT+CGMR`/`AT+CGPADDR`、PTY shell |

### 已隐藏 / 移除（FM350-GL 不支持）

下列功能在实机 `FM350.F32` 上无对应固件接口或受硬件形态限制，已在 WebUI 隐藏并在 `src/fm350.rs` 中显式返回不支持错误：

- 运营商配置（MBN 列表与切换，无 `+QMBNCFG`）
- SIM 卡热插拔检测开关（无 `+QSIMDET`）
- 数据漫游开关（`AT+GTROAM?` / `AT+EDEFROAM?` / `AT$ROAM?` / `AT+EROAMBAR?` 均返回 `ERROR`）
- 多小区 LTE 锁定与多频点 NR5G 列表锁定（`AT+EMMCHLCK` 每次仅接受单个 LTE 小区或单个 NR 小区/频点）
- IP 透传、USB 协议切换（`AT+GTUSBMODE=?` 仅支持 RNDIS `40,41`）、DMZ、LAN IP/DHCP 池、DNS 代理、设备信息页局域网 IP
- 在线 OTA 自更新（原更新器针对移远 `armv7` `/usrdata` 分区设计）
- 未使用的 `DRX / RX2 / RX3` 空白天线占位项（当值为 `-` 或 `None` 时不显示）

## Jetson 宿主机部署指南

### 1. udev 与 `usb0` 固定命名

创建 `/etc/udev/rules.d/99-fm350-gl.rules`（在 `06` 串口出现时加载 `rndis_host` 与 `option`、防止 `option` 抢占 RNDIS `00/01` 接口并释放 ADB 接口 `05`、固定 `/dev/fm350-at` → 接口 `06`）：

```udev
ACTION=="add", SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_interface", ATTR{bInterfaceNumber}=="06", ATTRS{idVendor}=="0e8d", ATTRS{idProduct}=="7127", RUN+="/usr/sbin/modprobe rndis_host", RUN+="/usr/sbin/modprobe option", RUN+="/bin/sh -c 'echo 0e8d 7127 > /sys/bus/usb-serial/drivers/option1/new_id 2>/dev/null || true'"
ACTION=="add|bind", SUBSYSTEM=="usb", DRIVER=="option", ATTR{bInterfaceNumber}=="00|01", ATTRS{idVendor}=="0e8d", ATTRS{idProduct}=="7127", RUN+="/bin/sh -c 'b=$$(basename $$(dirname /sys%p)); echo \"$$b:1.0\" > /sys/bus/usb/drivers/option/unbind 2>/dev/null || true; echo \"$$b:1.1\" > /sys/bus/usb/drivers/option/unbind 2>/dev/null || true; /usr/sbin/modprobe rndis_host 2>/dev/null || true; echo \"$$b:1.0\" > /sys/bus/usb/drivers/rndis_host/bind 2>/dev/null || true'"
ACTION=="add|bind", SUBSYSTEM=="usb", DRIVER=="option", ATTR{bInterfaceNumber}=="05", ATTRS{idVendor}=="0e8d", ATTRS{idProduct}=="7127", RUN+="/bin/sh -c 'echo %k > /sys/bus/usb/drivers/option/unbind 2>/dev/null || true'"
ACTION=="add|change", SUBSYSTEM=="tty", KERNEL=="ttyUSB*", ENV{ID_VENDOR_ID}=="0e8d", ENV{ID_MODEL_ID}=="7127", ENV{ID_USB_INTERFACE_NUM}=="06", SYMLINK+="fm350-at"
```

创建 `/etc/systemd/network/05-fm350-usb0.link` 与 `/etc/NetworkManager/conf.d/99-fm350-unmanaged.conf`：

```ini
# /etc/systemd/network/05-fm350-usb0.link
[Match]
Property=ID_VENDOR_ID=0e8d ID_MODEL_ID=7127

[Link]
Name=usb0
```

```ini
# /etc/NetworkManager/conf.d/99-fm350-unmanaged.conf
[keyfile]
unmanaged-devices=interface-name:usb0;interface-name:enx000011121314
```

重载配置并生效：

```bash
udevadm control --reload-rules
udevadm trigger --subsystem-match=usb --subsystem-match=tty --subsystem-match=net
systemctl reload NetworkManager
```

### 2. 部署模块内探针 `fm350-agent`

将静态编译的 `fm350-agent` 推送到模块 `/usr/bin/fm350-agent` 并注册 OpenWrt 自启动服务 `/etc/init.d/fm350_agent`：

```bash
adb push fm350-agent /usr/bin/fm350-agent
adb shell 'chmod 755 /usr/bin/fm350-agent'
adb shell 'cat << "EOF" > /etc/init.d/fm350_agent
#!/bin/sh /etc/rc.common
START=99
USE_PROCD=1

start_service() {
    procd_open_instance
    procd_set_param command /usr/bin/fm350-agent
    procd_set_param respawn
    procd_close_instance
}
EOF
chmod 755 /etc/init.d/fm350_agent
/etc/init.d/fm350_agent enable
/etc/init.d/fm350_agent restart'
```

### 3. 配置并启动宿主机 `simpleadmin-httpd` 服务

`/opt/simpleadmin/at_devices.conf`：

```text
/dev/fm350-at
```

`/etc/systemd/system/simpleadmin-httpd.service`：

```ini
[Unit]
Description=SimpleAdmin FM350-GL HTTP Administration Service
After=network-online.target systemd-udev-settle.service
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=/opt/simpleadmin
Environment=HOME=/root
ExecStart=/opt/simpleadmin/simpleadmin-httpd \
  --http :8080 --no-tls \
  --static /opt/simpleadmin/www \
  --auth-file /opt/simpleadmin/simpleadmin.auth \
  --ttl-file /opt/simpleadmin/ttlvalue \
  --at-devices-file /opt/simpleadmin/at_devices.conf \
  --chronyc /usr/bin/chronyc \
  --chrony-conf /etc/chrony/chrony.conf
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
```

启用服务并访问 `http://<宿主机IP>:8080`（首次默认账号密码为 `admin / admin`）：

```bash
systemctl daemon-reload
systemctl enable --now simpleadmin-httpd.service
```

如宿主机运行 `chrony` 并希望接入 FM350 GNSS 作为离线时间热备，在 `/etc/chrony/conf.d/geac91-gps.conf` 追加：

```text
refclock SOCK /run/chrony/fm350.sock refid FM35 offset 0.103 delay 0.5
```

修改后执行 `systemctl restart chrony.service` 即可。

## 构建与测试

在拥有交叉工具链（`clang` / `lld` / `aarch64-linux-musl`）的环境下构建：

```bash
# 代码检查与单元测试
cargo clippy --bins -- -D warnings
cargo test --bin simpleadmin-httpd

# 构建宿主机主程序与模块内 fm350-agent (aarch64-unknown-linux-musl)
cargo build --release --target aarch64-unknown-linux-musl --bin simpleadmin-httpd
rustc --target aarch64-unknown-linux-musl -C opt-level=z -C panic=abort -C strip=symbols -C lto=thin \
  scripts/fm350_agent.rs -o target/aarch64-unknown-linux-musl/release/fm350-agent

# 如需验证 installer crate，需同步更新 armv7 嵌入式负载及 SHA256SUMS
cargo build --release --target armv7-unknown-linux-musleabihf --bin simpleadmin-httpd
install -m 755 target/armv7-unknown-linux-musleabihf/release/simpleadmin-httpd development/simpleadmin/simpleadmin-httpd.armv7
bash scripts/checksums.sh
cargo test -p simpleadmin-installer
```

## 说明与许可

- 原项目与前端页面源自 [quectel-rgmii-toolkit-Rust](https://github.com/tcpqueue/quectel-rgmii-toolkit-Rust) 与 [quectel-rgmii-toolkit-Go](https://github.com/snjzb/quectel-rgmii-toolkit-Go)，保留 MIT 许可与第三方前端组件（ECharts、Lucide、ChroGPS Dash）版权声明，许可见 `development/simpleadmin/www/licenses/`。
