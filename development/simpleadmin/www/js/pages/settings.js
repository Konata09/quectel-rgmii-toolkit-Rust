function simpleSettings() {
      return {
        isLoading: false,
        showSuccess: false,
        showError: false,
        isClean: true,
        showModal: false,
        showImeiModal: false,
        isRebooting: false,
        atcmd: "",
        fetchATCommand: "",
        countdown: 0,
        atCommandResponse: "",
        // Height the output box was last fitted to; the user dragged it when it differs.
        atOutputFitted: 0,
        atOutputManual: false,
        ttldata: null,
        ttlvalue: 0,
        ttlStatus: false,
        newTTL: null,
        imei: "-",
        newImei: "",
        networkMessage: "",
        networkFailed: false,
        ttlMessage: "",
        isRebooted: true,
        language: SimpleAdmin.Lang ? SimpleAdmin.Lang.getCurrentLanguage() : "zh-CN",
        isSavingLanguage: false,
        languageSaveMessage: "",
        webUsername: "",
        webHttpPort: 80,
        webHttpEnabled: true,
        webuiCurrentPassword: "",
        isSavingWebui: false,
        webuiSaveMessage: "",
        webuiNewUrl: "",
        currentPassword: "",
        newPassword: "",
        confirmPassword: "",
        isSavingPassword: false,
        passwordSaveMessage: "",
        currentRootPassword: "",
        newRootPassword: "",
        confirmRootPassword: "",
        isSavingRootPassword: false,
        rootPasswordSaveMessage: "",
        rebootCountdownTimer: null,

        t(key) {
          return SimpleAdmin.Lang ? SimpleAdmin.Lang.t(key) : key;
        },

        fetchLanguageSetting() {
          if (!SimpleAdmin.Lang) return Promise.resolve(this.language);
          return SimpleAdmin.Lang.load().then((language) => {
            this.language = language;
            return language;
          });
        },

        saveLanguageSetting() {
          if (!SimpleAdmin.Lang) return;
          this.isSavingLanguage = true;
          this.languageSaveMessage = "";
          SimpleAdmin.Lang.setLanguage(this.language)
            .then((language) => {
              this.language = language;
              this.languageSaveMessage = this.t("已保存");
              setTimeout(() => {
                this.languageSaveMessage = "";
              }, 3000);
            })
            .catch((error) => {
              console.error("保存语言设置失败：", error);
              this.languageSaveMessage = this.t("保存失败");
            })
            .finally(() => {
              this.isSavingLanguage = false;
            });
        },

        async changeRootPassword() {
          if(this.isSavingRootPassword) return;
          this.rootPasswordSaveMessage = '';
          if(!this.currentRootPassword || !this.newRootPassword || this.newRootPassword !== this.confirmRootPassword) {
            this.rootPasswordSaveMessage = '请填写当前 root 密码，并确认两次新密码一致'; return;
          }
          this.isSavingRootPassword = true;
          const controller = new AbortController();
          const deadline = setTimeout(() => controller.abort(), 40000);
          try {
            const response = await fetch('/api/set_root_password', {method:'POST', signal:controller.signal, headers:{'Content-Type':'application/x-www-form-urlencoded'}, body:new URLSearchParams({current_password:this.currentRootPassword,new_password:this.newRootPassword,confirm_password:this.confirmRootPassword})});
            if(response.status === 401) {window.location.replace('/login.html');return;}
            const data = await response.json();
            if(!response.ok) throw new Error(data.error || '保存失败');
            this.currentRootPassword = this.newRootPassword = this.confirmRootPassword = '';
            window.location.replace('/login.html');
          } catch(error) {this.rootPasswordSaveMessage = error.name === 'AbortError' ? '请求超时，请确认设备密码与挂载状态' : error.message;}
          finally {clearTimeout(deadline);this.isSavingRootPassword=false;}
        },

        async fetchWebuiSetting() {
          try {
            const response = await fetch('/api/webui_settings', {cache:'no-store'});
            if (!response.ok) throw new Error(this.t("读取 WebUI 设置失败"));
            const data = await response.json();
            this.webUsername = data.username;
            this.webHttpPort = data.http_port;
            this.webHttpEnabled = data.http_enabled;
          } catch(error) { this.webuiSaveMessage = error.message; }
        },

        async saveWebuiPort() {
          if (this.isSavingWebui) return;
          this.webuiSaveMessage = ""; this.webuiNewUrl = "";
          if (!this.webuiCurrentPassword || !/^[1-9][0-9]{0,4}$/.test(String(this.webHttpPort)) || Number(this.webHttpPort) > 65535) {
            this.webuiSaveMessage = this.t("请填写当前 Web 密码和 1–65535 的端口"); return;
          }
          this.isSavingWebui = true;
          const controller = new AbortController();
          const deadline = setTimeout(() => controller.abort(), 40000);
          try {
            const response = await fetch('/api/set_webui_port', {method:'POST',signal:controller.signal,
              headers:{'Content-Type':'application/x-www-form-urlencoded'},
              body:new URLSearchParams({current_password:this.webuiCurrentPassword,http_port:String(this.webHttpPort)})});
            const data = await response.json();
            if (!response.ok) {
              const errors = {"current password incorrect":"当前密码不正确","HTTP port is already in use or unavailable":"端口已被占用或不可用，原配置保持不变"};
              throw new Error(this.t(errors[data.error] || data.error || "保存失败"));
            }
            this.webuiCurrentPassword = "";
            this.webuiSaveMessage = this.t(data.changed ? "端口已保存并生效，无需重启模块" : "端口未改变");
            if (data.warning) this.webuiSaveMessage += " · " + data.warning;
            if (data.changed) {
              if (["127.0.0.1","localhost","[::1]"].includes(window.location.hostname)) {
                this.webuiSaveMessage += " · " + this.t("通过 ADB 转发访问时，请在安装器重新打开管理页面");
              } else {
                const url = new URL(window.location.href);
                url.protocol = "http:"; url.port = String(data.http_port); url.pathname = "/login.html"; url.search = ""; url.hash = "";
                this.webuiNewUrl = url.href;
              }
            }
          } catch(error) { this.webuiSaveMessage = error.name === 'AbortError' ? this.t("请求超时，请重新检查当前端口") : error.message; }
          finally { clearTimeout(deadline); this.isSavingWebui = false; }
        },

        changeLoginPassword() {
          if (this.isSavingPassword) return;
          this.passwordSaveMessage = "";
          if (!this.currentPassword || !this.webUsername) {
            this.passwordSaveMessage = this.t("请输入当前密码和 Web 账号");
            return;
          }
          if (this.newPassword !== this.confirmPassword) {
            this.passwordSaveMessage = this.t("两次输入的新密码不一致");
            return;
          }

          this.isSavingPassword = true;
          return SimpleAdmin.Api.setPassword(this.currentPassword, this.newPassword, this.confirmPassword, this.webUsername)
            .then((res) => {
              return res.json().catch(() => ({})).then((data) => ({ res, data }));
            })
            .then(({ res, data }) => {
              if (!res.ok) {
                const error = data && data.error ? data.error : "";
                if (res.status === 403 || error === "current password incorrect") {
                  throw new Error("当前密码不正确");
                }
                if (error === "new password is empty") {
                  throw new Error("新密码不能为空");
                }
                if (error === "password confirmation mismatch") {
                  throw new Error("密码确认不一致");
                }
                throw new Error(error || "密码保存失败");
              }
              this.currentPassword = "";
              this.newPassword = "";
              this.confirmPassword = "";
              this.passwordSaveMessage = this.t("密码已保存，请使用新密码重新登录。");
              window.location.replace('/login.html');
            })
            .catch((error) => {
              console.error("保存登录密码失败：", error);
              this.passwordSaveMessage = this.t(error.message || "密码保存失败");
            })
            .finally(() => {
              this.isSavingPassword = false;
            });
        },

        closeModal() {
          this.showModal = false;
        },

        closeImeiModal() {
          this.showImeiModal = false;
        },

        showRebootModal() {
          this.showModal = true;
        },

        sendATCommand() {
          if (!this.atcmd) {
            this.atcmd = "ATI";
          }
          this.isLoading = true;
          // ★ 返回 fetch 的 Promise，这样外层可以 .then(...)
          return SimpleAdmin.Api.settingsData({ action: 'manual_at', command: this.atcmd })
            .then((data) => data.response || '')
            .then((data) => {
              this.atCommandResponse = data;
              this.isLoading = false;
              this.isClean = false;
              this.fitAtOutput();
              //this.fetchCurrentSettings();
              return data; // ★ 把结果再传下去，链式调用更方便
            })
            .catch((error) => {
              console.error("错误: ", error);
              this.showError = true;
              this.isLoading = false;
              throw error; // ★ 继续抛出，方便调用方捕获
            });
        },

        clearResponses() {
          this.atCommandResponse = "";
          this.isClean = true;
          this.fitAtOutput();
        },

        // Fits the output box to its text, between 200 px and 70 % of the window. Once the
        // user has dragged it to a size of their own it keeps that size and only grows.
        fitAtOutput() {
          this.$nextTick(() => {
            const box = document.getElementById('atOutputBox');
            if (!box || !box.offsetParent) return;
            const current = box.offsetHeight;
            if (this.atOutputFitted && Math.abs(current - this.atOutputFitted) > 2) this.atOutputManual = true;
            const max = Math.max(200, Math.round(window.innerHeight * 0.7));
            box.style.height = 'auto';
            const wanted = Math.min(max, Math.max(200, box.scrollHeight + box.offsetHeight - box.clientHeight));
            box.style.height = (this.atOutputManual ? Math.max(current, wanted) : wanted) + 'px';
            this.atOutputFitted = box.offsetHeight;
          });
        },

        startRebootCountdown(seconds = 40) {
          if (this.rebootCountdownTimer) {
            clearInterval(this.rebootCountdownTimer);
          }
          this.atCommandResponse = "";
          this.showModal = false;
          this.showImeiModal = false;
          this.isRebooting = true;
          this.countdown = seconds;
          this.isRebooted = false;  // 重启标志位，表示设备尚未重启完成

          // 进行倒计时
          this.rebootCountdownTimer = setInterval(() => {
            this.countdown--;
            if (this.countdown <= 0) {
              clearInterval(this.rebootCountdownTimer);
              this.rebootCountdownTimer = null;
              this.isRebooting = false;

              // 给设备一些时间重启后再执行初始化
              setTimeout(() => {
                this.isRebooted = true;  // 设置标志为已重启
                this.init();  // 重启后执行初始化（发送必要的 AT 命令）
              }, 5000);  // 延迟 5 秒，以确保设备完全重启
            }
          }, 1000);
        },

        handleRebootNotice(data) {
          if (!data || !(data.reboot || data.rebooting)) {
            return false;
          }
          const seconds = Number(data.rebootCountdownSeconds) || 40;
          this.startRebootCountdown(seconds);
          return true;
        },

        rebootDevice() {
          SimpleAdmin.Api.settingsData({ action: 'reboot' });
          this.startRebootCountdown(40);
        },


        openImeiModal() {
          const val = (this.newImei || '').trim();
          if (!val) {
            alert('没有提供新的 IMEI。');
            return;
          }
          if (val.length !== 15 || !/^\d+$/.test(val)) {
            alert('IMEI 无效');
            return;
          }
          if (this.imei !== '-' && val === this.imei) {
            alert('IMEI 与当前 IMEI 相同');
            return;
          }
          this.showImeiModal = true;
        },

        updateIMEI() {
          const val = (this.newImei || '').trim();
          this.showImeiModal = false;
          this.isLoading = true;
          SimpleAdmin.Api.settingsData({ action: 'set_imei', imei: val })
            .catch((error) => {
              console.info('设置 IMEI 后设备重启或连接断开，继续保持重启倒计时：', error);
            })
            .finally(() => {
              this.isLoading = false;
            });
          this.startRebootCountdown(40);
        },

        resetATCommands() {
          SimpleAdmin.Api.settingsData({ action: 'reset_at' });
          this.atCommandResponse = "";
          this.showRebootModal();
        },

        fetchCurrentSettings() {
          if (!this.isRebooted) {
            return;  // 如果设备还在重启，跳过
          }
          SimpleAdmin.Api.settingsData({ action: 'status' })
            .then((data) => {
              const oldImei = this.imei;
              const currentImei = (data.imei || '').trim();
              if (/^\d{14,17}$/.test(currentImei)) {
                this.imei = currentImei;
                if (!this.newImei || this.newImei === '-' || this.newImei === oldImei) {
                  this.newImei = currentImei;
                }
              }
            })
            .catch((error) => {
              console.error("错误: ", error);
              this.showError = true;
            });
        },

        fetchTTL() {
          SimpleAdmin.Api.getTTLStatus()
            .then((res) => {
              return res.json();
            })
            .then((data) => {
              this.ttldata = data;
              this.ttlStatus = this.ttldata.isEnabled;
              this.ttlvalue = this.ttldata.ttl;
            })
            .catch((error) => {
              console.error("Error fetching TTL status: ", error); // 日志：捕获错误
            });
        },

        setTTL() {
          const text = String(this.newTTL === null || this.newTTL === undefined ? "" : this.newTTL).trim();
          const ttlval = Number(text);
          this.ttlMessage = "";
          if (!/^\d{1,3}$/.test(text) || ttlval > 255) {
            this.ttlMessage = this.t("请输入 0–255 的 TTL 值");
            return;
          }

          this.isLoading = true;
          SimpleAdmin.Api.setTTL(ttlval)
            .then((res) => res.json().catch(() => ({})).then((data) => {
              if (!res.ok) throw new Error(data.error || "");
              this.ttlMessage = this.t("已保存");
            }))
            .catch((error) => {
              const detail = String((error && error.message) || "").trim();
              this.ttlMessage = this.t("保存失败") + (detail ? " · " + detail : "");
            })
            .finally(() => {
              this.fetchTTL();
              this.isLoading = false;
            });
        },

        // ---------- 断网自动恢复（看门狗） ----------
        watchdog: null,
        watchdogForm: null,
        watchdogDirty: false,
        watchdogSaving: false,
        watchdogMessage: '',
        watchdogFailed: false,
        watchdogTimer: null,
        watchdogSteps: [
          { key: 'reregister', tier: 1, name: '重新注册网络', note: 'AT+COPS=2 后 AT+COPS=0，约 45 秒内检查是否恢复。影响最小。' },
          { key: 'radio', tier: 2, name: '重启射频', note: 'AT+CFUN=0 / 1，约 1 分钟。小区锁定若被模块清除，会自动重新应用。' },
          { key: 'sim_switch', tier: 3, name: '切换 SIM 卡槽', note: '切到另一卡槽，90 秒内未恢复则切回原卡槽。只有一张卡时不建议开启。' },
          { key: 'reboot', tier: 4, name: '重启模块', note: 'AT+CFUN=1,1。恢复联网后重启计数清零。' }
        ],
        async watchdogLoad() {
          clearTimeout(this.watchdogTimer);
          try {
            const data = await SimpleAdmin.Api.networkData({ action: 'watchdog' });
            this.watchdog = data;
            if (!this.watchdogDirty || !this.watchdogForm) {
              this.watchdogForm = Object.assign({}, data.settings);
              this.watchdogDirty = false;
            }
          } catch (error) {
            this.watchdogFailed = true;
            this.watchdogMessage = this.t('读取失败') + ' · ' + error.message;
          }
          // Follow a recovery in progress while the page is open.
          if (document.querySelector('#page-settings.active') && !document.hidden) {
            this.watchdogTimer = setTimeout(() => this.watchdogLoad(), 10000);
          }
        },
        watchdogEdit() {
          this.watchdogDirty = true;
          this.watchdogMessage = '';
        },
        watchdogStepName(key) {
          const step = this.watchdogSteps.find((s) => s.key === key);
          return step ? this.t(step.name) : key;
        },
        watchdogPhaseText() {
          const w = this.watchdog;
          if (!w) return '';
          const en = this.language !== 'zh-CN';
          const minutes = Math.ceil((w.cooldown_seconds || 0) / 60);
          const texts = {
            off: ['已关闭', 'Off'],
            no_probe: ['延迟监测已关闭，无法判断断网', 'Latency monitoring is off; outages cannot be detected'],
            startup: ['开机等待中', 'Waiting after startup'],
            monitoring: ['监测中 · 网络正常', 'Watching · connection OK'],
            outage: [`断网 ${w.outage_seconds} 秒 / ${w.settings.outage_seconds} 秒`, `Offline ${w.outage_seconds} s / ${w.settings.outage_seconds} s`],
            recovering: [`正在执行第 ${w.tier} 级 · ${this.watchdogStepName(w.step)}`, `Running step ${w.tier} · ${this.watchdogStepName(w.step)}`],
            cooldown: [minutes ? `冷却中 · ${minutes} 分钟` : '冷却中', minutes ? `Cooling down · ${minutes} min` : 'Cooling down']
          };
          let text = (texts[w.phase] || [w.phase, w.phase])[en ? 1 : 0];
          if (w.reboots) text += en ? ` · ${w.reboots} restarts in a row` : ` · 已连续重启 ${w.reboots} 次`;
          return text;
        },
        async watchdogSave() {
          const f = this.watchdogForm;
          if (!f || this.watchdogSaving) return;
          const outage = Math.round(Number(f.outage_seconds));
          const cooldown = Math.round(Number(f.cooldown_minutes));
          const reboots = Math.round(Number(f.max_reboots));
          this.watchdogFailed = true;
          if (!(outage >= 30 && outage <= 3600)) { this.watchdogMessage = this.t('断网时间为 30–3600 秒'); return; }
          if (!(cooldown >= 5 && cooldown <= 240)) { this.watchdogMessage = this.t('间隔为 5–240 分钟'); return; }
          if (!(reboots >= 1 && reboots <= 10)) { this.watchdogMessage = this.t('重启次数为 1–10 次'); return; }
          if (f.enabled && !this.watchdogSteps.some((s) => f[s.key])) { this.watchdogMessage = this.t('请至少开启一个恢复步骤'); return; }
          if (f.enabled && f.reboot && !window.confirm(this.t('已允许看门狗重启模块。断网时模块可能自动重启，确定保存？'))) { this.watchdogFailed = false; this.watchdogMessage = ''; return; }
          this.watchdogSaving = true;
          this.watchdogMessage = '';
          try {
            const flag = (v) => (v ? '1' : '0');
            const data = await SimpleAdmin.Api.networkData({
              action: 'watchdog_save', enabled: flag(f.enabled), outage_seconds: String(outage), cooldown_minutes: String(cooldown),
              reregister: flag(f.reregister), radio: flag(f.radio), sim_switch: flag(f.sim_switch), reboot: flag(f.reboot), max_reboots: String(reboots)
            });
            if (!data || data.ok === false) throw new Error((data && data.error) || '');
            this.watchdog = data;
            this.watchdogForm = Object.assign({}, data.settings);
            this.watchdogDirty = false;
            this.watchdogFailed = false;
            this.watchdogMessage = this.t('已保存');
          } catch (error) {
            this.watchdogMessage = this.t('保存失败') + (error.message ? ' · ' + error.message : '');
          } finally {
            this.watchdogSaving = false;
          }
        },

        init() {
          if (!this.isRebooted) {
            return;  // 如果设备正在重启，跳过
          }

          this.fetchWebuiSetting();
          this.fetchLanguageSetting();  // 获取界面语言设置
          this.fetchCurrentSettings();  // 发送 AT 命令获取当前设置
          this.fetchTTL();  // 获取 TTL 状态
          this.watchdogLoad();
          // Pages mount once; reload the watchdog state on every return to this page.
          window.addEventListener('simpleadmin:page-changed', (event) => {
            if (event.detail && event.detail.page === 'settings') this.watchdogLoad();
          });
        },
      };
    }

    if (window.SimpleAdminSpaMode) {
      (window.SimpleAdmin.Pages = window.SimpleAdmin.Pages || {}).settings = simpleSettings;
    } else {
      mountSimpleAdminVueApp(simpleSettings);
    }
