# GNSS 授时

模块装有 gnss-ppsd 一类的 1PPS 授时程序和 chrony 时，“设备 → GNSS 授时”显示 chrony 的同步状态、卫星天空图与信噪比，以及 24 小时的授时历史。页面使用 [ChroGPS Dash](https://chro.gpsda.sh/) 的前端（MIT），由本程序提供数据，模块上不需要 PHP、gpsd 或另一个网页服务。页面不显示标题，仪表盘铺满侧边栏右侧（手机上为顶栏与底部标签栏之间），在框内滚动。

没有找到 chronyc（默认 `/opt/bin/chronyc`）时，菜单里不显示这一项，后台也不采集；直接打开 `#gnss` 只显示一条说明。chrony 和 1PPS 的安装不由本程序负责。

> 已在 macOS 上用模拟数据和原脚本的输出做过对照测试；尚未在 RM520N-GL 真机上运行本程序。

## 采集

| 内容 | 周期 | 来源 |
| --- | --- | --- |
| 同步状态、时间源、服务器统计 | 10 秒 | `chronyc -n tracking / sources / sourcestats / serverstats`，`chrony.conf` |
| 位置、定位状态、DOP、卫星 | 10 秒 | `AT+QGPSGNMEA="GGA"`、`"GSA"`、`"GSV"`，经本程序的 AT 队列 |
| 时钟跳变次数 | 10 秒 | `/var/log/messages` 中 chronyd 的 `System clock was stepped by`，只读新增部分 |
| NTP 客户端 | 30 秒 | `chronyc -n clients` |
| 历史（偏差、频率、PPS、卫星数、信噪比、DOP） | 30 秒 | 以上数据；chrony 第一次同步之后才开始记录 |

- 历史保存最近 24 小时，只在内存中（约 230 KB），程序重启后清空，不写 NAND。每颗卫星的信噪比只保留最近 1 小时，用于 1 小时图和卫星迷你走势；6 小时、24 小时的星座信噪比图按星座平均值保存。
- 开机时系统时钟可能相差几个月，所以历史从 chrony 第一次同步后开始，避免时间轴被拉长。
- NMEA 查询与其他页面共用 AT 队列，会出现在 AT 终端页的 AT 队列中。每条查询最多等 4 秒；小区扫描占用 AT 口时，这一轮只更新 chrony 部分，卫星数据保持上一轮。
- GNSS 关闭时 `AT+QGPSGNMEA` 返回错误，页面显示没有卫星；需要 `AT+QGPSCFG="glonassnmeatype"`、`"galileonmeatype"`、`"beidounmeatype"` 打开对应系统的 NMEA 输出，否则只有 GPS。

## 接口

都需要登录，只接受 GET。

| 地址 | 返回 |
| --- | --- |
| `/api/gnss` | `installed`（是否找到 chronyc）、`synced`、`started`（第一次同步，UTC）、`updated`、`samples` |
| `/api/gnss?part=live` | ChroGPS Dash `?ajax=1` 的数据（不含图表），另有 `node`（模块型号）；还没有采集到时为 503 |
| `/api/gnss?part=hist1` / `hist6` / `hist24` | 1、6、24 小时历史，文本格式；6 小时每 3 点取 1 点，24 小时每 12 点取 1 点 |
| `/api/gnss?part=clients` | NTP 客户端列表 |

未安装时，带 `part` 的请求返回 404。

启动参数 `--chronyc` 和 `--chrony-conf` 可改 chronyc 与配置文件的位置（默认 `/opt/bin/chronyc`、`/opt/etc/chrony/chrony.conf`）。

## 与 gnss-ppsd 自带仪表盘的区别

gnss-ppsd 原来的仪表盘由 `gnss-dash-poll`（shell + awk）采集，lighttpd 提供静态文件。移到本程序后：

- 需要与其他页面相同的登录；会话失效时整页跳到登录页。
- 不再需要 `gnss-dash.service`、`gnss-dash-poll` 和 `www/gnss` 软链接。安装或在线更新会整体替换 `www/`，原来的软链接本来也会被删掉。
- NMEA 改经本程序直接打开的 `/dev/smd11` 读取。`gnss-dash-poll` 写的是 `/dev/ttyOUT`，安装程序会清理连接 `/dev/ttyIN`、`/dev/ttyOUT` 的旧 socat 桥，所以旧采集脚本在本程序下无法工作。
- 仪表盘自己的设置（主题、刷新间隔、面板顺序等）存在浏览器的 `gnss.` 前缀下，不影响本程序的外观设置。未在仪表盘里选主题时，跟随本程序的浅色 / 深色。
- 6 小时、24 小时的点按样本序号抽取，新样本进来时已有的点不移动。
- 页面头部的节点名显示模块型号。

gnss-ppsd 本身每 5 分钟通过 `/bin/atcmd` 读一次 RMC 校对整秒。安装程序不清理 `/dev/ttyOUT2` 等其他桥；这条路径在本程序下是否可用尚未在真机验证。

从旧仪表盘迁移时，停用旧的采集服务：

```sh
systemctl disable --now gnss-dash
mount -o remount,rw /
rm -f /lib/systemd/system/multi-user.target.wants/gnss-dash.service /lib/systemd/system/gnss-dash.service
sync
mount -o remount,ro /
rm -rf /var/tmp/gnss-dash
```

## 前端

`development/simpleadmin/www/gnss/index.html` 由 `vendor/chrogps-dash/build.py` 生成：取 ChroGPS Dash 真实安装渲染出的页面（`rendered.html`），去掉为原机器渲染的图表，插入 `shim.js`。`shim.js` 拦截页面的 `?ajax=` 请求，从 `/api/gnss` 取数据，并在浏览器中绘制原来由 PHP 生成的图表，模块不做绘图。修改 `shim.js` 后重新运行：

```sh
python3 vendor/chrogps-dash/build.py
```

管理面板、在线更新等依赖 PHP 的功能已隐藏。NMEA 没有 TDOP 和 EPT；模块没有网络 NTP 对端，“NTP Measurements”图为空。

## 测试

- `cargo test gnss`：用 `tests/fixtures/gnss/` 中按 RM520N-GL 实测状态还原的 chronyc 与 NMEA 回复，把本程序的输出与 gnss-ppsd 原 awk 脚本（`reference.awk`）的输出逐项比较。修改输入后运行 `make-inputs.py` 和 `regenerate.sh` 重新生成期望值。
- `node tests/gnss-browser.cjs`：模拟模式下检查菜单、框架内的实时数据和图表、设置隔离、外观跟随、手机布局、英文界面、未安装提示和会话失效跳转。模拟模式启动时预填 6 小时历史。
