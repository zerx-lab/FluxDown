//! agent Doctor 探测、就地修复命令与日志导出。
//!
//! 每个检查项是一次只读探测：NMH 中继/清单/浏览器注册、本进程 IPC 与网关监听、
//! 兼容 HTTP API、daemon RPC、URL scheme 与 `.torrent` 关联、日志目录可写。
//! 权限类检查是动态的：daemon 在下载实际发生的进程里真实写入每个下载目录 / 数据目录并运行
//! 外部组件（`daemon.diagnostics.probe`），agent 像浏览器那样拉起 NMH 中继；桌面宿主另读开机
//! 自启与系统通知的系统级开关。
//! 修复动作是显式的第二步（`repair`，实现见 [`repair`] 子模块），探测本身从不改动配置（写入
//! 探测只留下随即删除的临时文件）。需要管理员授权的修复只「释放」旧权限（见
//! [`crate::permission`]）。打开日志目录仅接受已存在的 agent/daemon 日志目录或 agent 数据目录。

mod repair;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxdown_protocol::capture_link::OpenAssociation;
use fluxdown_protocol::{
    AgentSnapshot, CUSTOM_CATEGORIES_PREF_KEY, ComponentProbeDto, CustomCategoryDto,
    DiagnosticCheckDto, DiagnosticLevel, DiagnosticRepairParams, DiagnosticsProbeParams,
    DiagnosticsProbeResult, DiagnosticsReportDto, LogExportParams, LogExportResult, LogPathsDto,
    PlatformIntegrationDto, StorageProbeDto, StorageProbeFailure, StorageProbeRole,
    StorageProbeTarget,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::daemon_client::{DaemonClient, DaemonClientConfig};
use crate::event_hub::AgentEventHub;
use crate::state::{AgentState, StateStore};
use crate::supervisor::DaemonSupervisor;

/// 检查项 id；UI 据此映射 `doctorCheck{Camel}` 文案与修复按钮。
const CHECK_NMH_BINARY: &str = "nmh_binary";
const CHECK_NMH_MANIFEST: &str = "nmh_manifest";
const CHECK_NMH_BROWSER: &str = "nmh_browser";
const CHECK_NMH_RELAY: &str = "nmh_relay";
const CHECK_APP_LISTENER: &str = "app_listener";
const CHECK_LOCAL_SERVER: &str = "local_server";
const CHECK_DAEMON: &str = "daemon";
const CHECK_URL_PROTOCOL: &str = "url_protocol";
const CHECK_TORRENT_ASSOCIATION: &str = "torrent_association";
const CHECK_LOG_DIR: &str = "log_dir";
const CHECK_DAEMON_STARTUP: &str = "daemon_startup";
/// 空闲时仍会周期性访问硬盘的来源（做种、RSS 定时抓取、空闲文件扫描）。
const CHECK_DISK_SLEEP: &str = "disk_sleep";
/// daemon 动态写入探测：全局默认 / 队列 / RSS 源 / 分类保存目录与 daemon 数据目录。
const CHECK_SAVE_DIR: &str = "save_dir";
const CHECK_QUEUE_SAVE_DIR: &str = "queue_save_dir";
const CHECK_RSS_SAVE_DIR: &str = "rss_save_dir";
const CHECK_CATEGORY_SAVE_DIR: &str = "category_save_dir";
const CHECK_DATA_DIR: &str = "data_dir";
/// daemon 动态探测本身失败（超时 / 不支持）：目录与组件都没有结果。
const CHECK_PERMISSION_PROBE: &str = "permission_probe";
/// 外部组件（ffmpeg / ffprobe / yt-dlp）能否被 daemon 实际运行。
const CHECK_COMPONENT: &str = "component";
/// 桌面宿主：开机自启是否被系统级开关禁用。
const CHECK_AUTOSTART: &str = "autostart";
/// 桌面宿主：系统是否允许显示下载完成通知。
const CHECK_NOTIFICATIONS: &str = "notifications";
/// 桌面宿主：像浏览器那样拉起已注册的 NMH 入口并经它连到本 agent。
const CHECK_NMH_LAUNCH: &str = "nmh_launch";
/// Windows：限制原生消息主机的组织策略。
const CHECK_NMH_POLICY: &str = "nmh_policy";
/// Windows：浏览器经 `%ComSpec% /d /c` 启动原生消息主机所用的命令解释器。
const CHECK_NMH_COMSPEC: &str = "nmh_comspec";
/// 类 Unix：属于其他用户的 NMH 启动脚本 / 清单（以后的自动注册改不了）。
const CHECK_NMH_OWNERSHIP: &str = "nmh_ownership";
/// 桌面宿主以提权身份运行：类 Unix 为 root；Windows 为 UAC 完整管理员令牌（浏览器中继连不上
/// 本进程的命名管道，经它打开的文件也以管理员身份运行）。
#[cfg(any(unix, windows))]
const CHECK_ELEVATED_RUN: &str = "elevated_run";

/// 提示码；UI 映射 `doctorHint{Camel}`。
const HINT_REINSTALL_APP: &str = "reinstall_app";
const HINT_REREGISTER_NMH: &str = "reregister_nmh";
const HINT_NMH_OTHER_INSTALL: &str = "nmh_other_install";
const HINT_RESTART_APP: &str = "restart_app";
const HINT_ENABLE_LOCAL_SERVER: &str = "enable_local_server";
const HINT_CHECK_FIREWALL: &str = "check_firewall";
const HINT_ENABLE_PROTOCOL: &str = "enable_protocol";
/// 用户已在设置中关闭该关联，但系统里没有其他可接手的程序（macOS 回落到 FluxDown）。
const HINT_ASSOCIATION_OFF: &str = "association_off";
const HINT_CHECK_DISK: &str = "check_disk";
/// daemon 反复启动即退出（端口无法监听、初始化失败）；详情带 stderr 摘要。
const HINT_DAEMON_STARTUP_FAILED: &str = "daemon_startup_failed";
/// 存在阻止硬盘休眠的来源；详情逐项列出数量与间隔。
const HINT_DISK_SLEEP_BLOCKERS: &str = "disk_sleep_blockers";
/// 目录无写入权限（属主 / ACL / 容器 uid 不符）。
const HINT_DIR_PERMISSION: &str = "dir_permission";
/// macOS：「下载」「桌面」「文稿」、外置与网络卷受隐私授权（TCC）保护。
const HINT_DIR_PERMISSION_MACOS: &str = "dir_permission_macos";
const HINT_DIR_READ_ONLY: &str = "dir_read_only";
/// 目录所在卷剩余空间不足（或配额已满）。
const HINT_LOW_DISK_SPACE: &str = "low_disk_space";
/// 目录与所有上级都不存在：移动硬盘 / 网络盘未挂载或路径写错。
const HINT_DIR_MISSING: &str = "dir_missing";
/// 文件调用在时限内没有返回：网络盘失联或磁盘无响应。
const HINT_DIR_TIMEOUT: &str = "dir_timeout";
/// 组件启动被系统拒绝：缺执行权限、noexec 挂载、隔离属性或安全软件拦截。
const HINT_COMPONENT_BLOCKED: &str = "component_blocked";
/// 同上，但这是 FluxDown 托管安装的组件，可点「修复执行权限」。
const HINT_COMPONENT_BLOCKED_FIXABLE: &str = "component_blocked_fixable";
/// daemon 未在时限内完成动态探测。
const HINT_PERMISSION_PROBE_FAILED: &str = "permission_probe_failed";
/// 组件能启动但运行失败：文件损坏、架构不符或缺运行库。
const HINT_COMPONENT_BROKEN: &str = "component_broken";
/// 自启条目在，但被系统设置 / 任务管理器 / 桌面环境禁用。
const HINT_AUTOSTART_BLOCKED: &str = "autostart_blocked";
const HINT_NOTIFICATIONS_BLOCKED: &str = "notifications_blocked";
/// Linux 会话里没有通知服务。
const HINT_NOTIFICATIONS_UNAVAILABLE: &str = "notifications_unavailable";
/// macOS 授权状态不可读，只能发送测试通知目视确认。
const HINT_NOTIFICATIONS_UNVERIFIED: &str = "notifications_unverified";
/// 浏览器式拉起 NMH 入口失败。
const HINT_NMH_LAUNCH_FAILED: &str = "nmh_launch_failed";
const HINT_NMH_POLICY_BLOCKED: &str = "nmh_policy_blocked";
const HINT_NMH_USER_HOSTS_DISABLED: &str = "nmh_user_hosts_disabled";
const HINT_NMH_CMD_DISABLED: &str = "nmh_cmd_disabled";
const HINT_NMH_FOREIGN_OWNED: &str = "nmh_foreign_owned";
/// `ComSpec` 配置值不可用：浏览器起不来原生消息主机。
const HINT_NMH_COMSPEC_INVALID: &str = "nmh_comspec_invalid";
/// `ComSpec` 已正确，但本进程（以及修改前启动的浏览器）仍沿用旧值。
const HINT_NMH_COMSPEC_RESTART: &str = "nmh_comspec_restart";
#[cfg(unix)]
const HINT_RUNNING_ELEVATED: &str = "running_elevated";
#[cfg(windows)]
const HINT_RUNNING_AS_ADMIN: &str = "running_as_admin";

/// 修复动作；UI 映射 `doctorAction{Camel}`，并作为 `repair` 的 `action`。
pub const ACTION_REREGISTER: &str = "reregister";
/// 把由另一份 FluxDown 提供的 NMH 注册改指向本安装；执行上与 `reregister` 相同。
pub const ACTION_USE_THIS_INSTALL: &str = "use_this_install";
pub const ACTION_ENABLE_SERVICE: &str = "enable_service";
pub const ACTION_REGISTER: &str = "register";
pub const ACTION_OPEN_LOG_DIR: &str = "open_log_dir";
pub const ACTION_REFRESH_TRACKERS: &str = "refreshTrackers";
pub const ACTION_REFRESH_ED2K_SERVERS: &str = "refreshEd2kServers";
/// 发送一条测试通知（只在桌面宿主可用）。
pub const ACTION_TEST_NOTIFICATION: &str = "test_notification";
/// 目录写入被拒：改回属主 / 加 ACL（必要时请求管理员授权），`target` 为目录配置路径。
pub const ACTION_FIX_DIR_ACCESS: &str = "fix_dir_access";
/// 补上托管组件的执行权限，`target` 为组件名。
pub const ACTION_FIX_COMPONENT: &str = "fix_component";
/// 重新启用被系统级开关禁用的开机自启。
pub const ACTION_ENABLE_AUTOSTART: &str = "enable_autostart";
/// 打开系统设置页，`target` 为 [`crate::platform::SettingsPane::name`]。
pub const ACTION_OPEN_SETTINGS: &str = "open_settings";
/// 把 `ComSpec` 改回可用值（只写用户变量，无需管理员）并通知资源管理器刷新环境变量。
pub const ACTION_FIX_COMSPEC: &str = "fix_comspec";
/// `register` 动作的 `.torrent` 关联目标。
pub const TARGET_TORRENT: &str = "torrent";

const URL_SCHEMES: [&str; 3] = ["fluxdown", "magnet", "ed2k"];

/// 回环/IPC 探测超时：慢回答本身就是结论。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_TIMEOUT: Duration = Duration::from_secs(5);
/// NMH 中继日志导出上限。
const NMH_LOG_EXPORT_BYTES: u64 = 4 * 1024 * 1024;
/// daemon 动态探测的总时限（daemon 内单目录 10s、单组件 20s，彼此并发）。
const PERMISSION_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// 剩余空间低于该值报警告：大文件下载、BT 预分配与 mux 中间文件都会在此之前失败。
const LOW_SPACE_BYTES: u64 = 1024 * 1024 * 1024;

pub struct DiagnosticsService {
    daemon: Arc<DaemonClient>,
    daemon_config: DaemonClientConfig,
    events: AgentEventHub,
    state: Arc<Mutex<AgentState>>,
    store: Arc<StateStore>,
    api_switches: Arc<fluxdown_api::server::ApiRuntimeSwitches>,
    api_token: fluxdown_api::auth::TokenCell,
    /// daemon 拉起监管与其 stderr 落盘文件；未接线（精简宿主 / 测试）时不产出启动检查。
    startup: Option<DaemonStartupProbe>,
    /// 桌面宿主的系统集成探测（开机自启、系统通知）；headless / 测试宿主为 `None`。
    desktop: Option<DesktopProbe>,
    /// 嵌入宿主（移动端进程内 agent）：没有 TCP 网关、NMH IPC 与兼容 HTTP API，也不跑在会休眠的
    /// 机械盘上，`app_listener` / `local_server` / `disk_sleep` 不适用。
    embedded: bool,
    /// 修复串行执行：多个界面连接同时点修复时不会叠出多个授权对话框，也不会交错改写注册。
    repair_lock: Mutex<()>,
}

struct DaemonStartupProbe {
    supervisor: Arc<DaemonSupervisor>,
    stderr_log: PathBuf,
}

struct DesktopProbe {
    notifier: Arc<crate::notification::Notifier>,
    text: crate::notification::NoticeText,
}

impl DiagnosticsService {
    #[must_use]
    pub fn new(
        daemon: Arc<DaemonClient>,
        daemon_config: DaemonClientConfig,
        events: AgentEventHub,
        state: Arc<Mutex<AgentState>>,
        store: Arc<StateStore>,
        api_switches: Arc<fluxdown_api::server::ApiRuntimeSwitches>,
        api_token: fluxdown_api::auth::TokenCell,
    ) -> Self {
        Self {
            daemon,
            daemon_config,
            events,
            state,
            store,
            api_switches,
            api_token,
            startup: None,
            desktop: None,
            embedded: false,
            repair_lock: Mutex::new(()),
        }
    }

    /// 接入 daemon 监管：Doctor 据此暴露「启动即崩溃 / 无法监听」的崩溃循环与 stderr 摘要。
    #[must_use]
    pub fn with_daemon_startup(
        mut self,
        supervisor: Arc<DaemonSupervisor>,
        stderr_log: PathBuf,
    ) -> Self {
        self.startup = Some(DaemonStartupProbe {
            supervisor,
            stderr_log,
        });
        self
    }

    /// 桌面宿主：Doctor 额外检查开机自启与系统通知，并提供「发送测试通知」。
    #[must_use]
    pub fn with_desktop_checks(mut self, notifier: Arc<crate::notification::Notifier>) -> Self {
        self.desktop = Some(DesktopProbe {
            notifier,
            text: crate::notification::NoticeText::default(),
        });
        self
    }

    /// 嵌入宿主：省略网关监听、兼容 HTTP API 与硬盘休眠检查（这些设施在进程内宿主不存在）。
    #[must_use]
    pub fn for_embedded_host(mut self) -> Self {
        self.embedded = true;
        self
    }

    /// 运行全部探测并生成报告。
    pub async fn run(&self) -> Result<DiagnosticsReportDto, DiagnosticsError> {
        let gateway = self.state.lock().await.gateway.clone();
        let data_dir = self.store.data_dir().to_path_buf();
        let opted_out = self.events.inspect(opted_out_associations);
        let disk_sleep = if self.embedded {
            None
        } else {
            self.events.inspect(disk_sleep_blockers)
        };
        let category_dirs = self.events.inspect(category_probe_targets);
        let sync_probe = tokio::task::spawn_blocking(move || probe_sync(&data_dir, &opted_out))
            .await
            .map_err(join_error)?;
        let gateway_probes = async {
            if self.embedded {
                None
            } else {
                Some(tokio::join!(
                    probe_listener(gateway.port),
                    probe_local_server(&gateway)
                ))
            }
        };
        let (daemon, gateway_checks, daemon_startup, permissions, desktop, nmh_launch) = tokio::join!(
            self.probe_daemon(),
            gateway_probes,
            self.probe_daemon_startup(),
            self.probe_permissions(category_dirs),
            self.probe_desktop(),
            self.probe_nmh_launch(&sync_probe.nmh_diagnosis),
        );

        let mut checks = sync_probe.nmh;
        checks.extend(nmh_launch);

        if let Some((listener, local_server)) = gateway_checks {
            checks.push(listener);
            checks.push(local_server);
        }
        checks.push(daemon.check);
        if let Some(check) = daemon_startup {
            checks.push(check);
        }
        if let Some(blockers) = disk_sleep {
            checks.push(disk_sleep_check(&blockers));
        }
        match permissions {
            Ok(result) => checks.extend(permission_checks(
                &result,
                self.desktop.is_some(),
                crate::permission::Os::CURRENT,
            )),
            // daemon 不可达时上面的 daemon 检查已报错，不再重复。
            Err(error) if daemon.connected => checks.push(check(
                CHECK_PERMISSION_PROBE,
                "",
                DiagnosticLevel::Error,
                format!("permission probe failed: {error}"),
                HINT_PERMISSION_PROBE_FAILED,
                None,
            )),
            Err(_) => {}
        }
        checks.extend(sync_probe.shell);
        checks.extend(desktop);
        checks.push(sync_probe.log_dir);

        let attention = checks
            .iter()
            .filter(|check| matches!(check.level, DiagnosticLevel::Warn | DiagnosticLevel::Error))
            .count();
        tracing::info!(checks = checks.len(), attention, "doctor run completed");

        Ok(DiagnosticsReportDto {
            generated_at_unix_ms: unix_ms(),
            app_version: fluxdown_protocol::APP_VERSION.to_owned(),
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            agent_data_dir: self.store.data_dir().display().to_string(),
            daemon_connected: daemon.connected,
            checks,
        })
    }

    /// 执行修复动作；成功返回 `{ok:true}` 或 daemon RPC 的返回值。
    pub async fn repair(&self, params: &DiagnosticRepairParams) -> Result<Value, DiagnosticsError> {
        let _serial = self.repair_lock.lock().await;
        match params.action.as_str() {
            ACTION_REREGISTER | ACTION_USE_THIS_INSTALL => self.repair_nmh().await,
            ACTION_FIX_COMSPEC => self.repair_comspec().await,
            ACTION_FIX_DIR_ACCESS => self.repair_dir_access(&params.target).await,
            ACTION_FIX_COMPONENT => self.repair_component(&params.target).await,
            ACTION_ENABLE_AUTOSTART => self.repair_autostart().await,
            ACTION_OPEN_SETTINGS => self.open_settings(&params.target).await,
            ACTION_ENABLE_SERVICE => {
                self.enable_service().await?;
                Ok(json!({ "ok": true }))
            }
            ACTION_TEST_NOTIFICATION => {
                self.send_test_notification().await?;
                Ok(json!({ "ok": true }))
            }
            ACTION_REGISTER => {
                let target = params.target.trim().to_owned();
                if target == TARGET_TORRENT {
                    spawn_blocking_platform(|| crate::platform::set_file_association(true)).await?;
                } else if URL_SCHEMES.contains(&target.as_str()) {
                    spawn_blocking_platform(move || {
                        crate::platform::set_url_protocol(&target, true)
                    })
                    .await?;
                } else {
                    return Err(DiagnosticsError::InvalidAction(format!(
                        "register target: {target}"
                    )));
                }
                Ok(json!({ "ok": true }))
            }
            ACTION_OPEN_LOG_DIR => {
                let data_dir = self.store.data_dir().to_path_buf();
                let target = if params.target.trim().is_empty() {
                    data_dir.clone()
                } else {
                    PathBuf::from(params.target.trim())
                };
                let paths = self.log_paths().await;
                let mut allowed = vec![data_dir, PathBuf::from(paths.agent_log_dir)];
                if !paths.daemon_log_dir.is_empty() {
                    allowed.push(PathBuf::from(paths.daemon_log_dir));
                }
                spawn_blocking_platform(move || {
                    let target = resolve_log_dir_target(&target, &allowed).ok_or_else(|| {
                        crate::platform::PlatformError::Failed(
                            "open_log_dir target must be an existing log or data directory"
                                .to_owned(),
                        )
                    })?;
                    crate::platform::open_path(&target, false)
                })
                .await?;
                Ok(json!({ "ok": true }))
            }
            ACTION_REFRESH_TRACKERS => {
                self.daemon_call(fluxdown_protocol::method::DAEMON_BT_TRACKER_SUBSCRIPTION_REFRESH)
                    .await
            }
            ACTION_REFRESH_ED2K_SERVERS => {
                self.daemon_call(fluxdown_protocol::method::DAEMON_ED2K_SERVER_SUBSCRIPTION_REFRESH)
                    .await
            }
            other => Err(DiagnosticsError::InvalidAction(other.to_owned())),
        }
    }

    /// agent 与 daemon 的日志目录；daemon 不可达时其目录为空串。
    pub async fn log_paths(&self) -> LogPathsDto {
        let describe = self.daemon_describe().await;
        crate::log_export::log_paths(
            &crate::log_export::agent_log_dir(self.store.data_dir()),
            daemon_log_dir(describe.as_ref()),
        )
    }

    /// 打包 agent 摘要、Doctor 报告、daemon 快照与两侧日志到 `.zip`。
    pub async fn export_logs(
        &self,
        params: &LogExportParams,
    ) -> Result<LogExportResult, DiagnosticsError> {
        let target = crate::log_export::resolve_target(&params.target_path)
            .map_err(|message| DiagnosticsError::InvalidAction(message.to_owned()))?;
        let mut zip = crate::log_export::ZipWriter::new();

        let report = self.run().await?;
        zip.add("agent/diagnostics.json", &pretty_json(&report));
        zip.add(
            "agent/summary.json",
            &pretty_json(&self.agent_summary().await),
        );

        let describe = self.daemon_describe().await;
        match &describe {
            Some(describe) => zip.add("daemon/describe.json", &pretty_json(describe)),
            None => zip.add(
                "daemon/describe.json",
                b"{\"error\":\"daemon unreachable\"}\n",
            ),
        }
        match self.fetch_daemon_export().await {
            Ok(bytes) => zip.add(
                "daemon/snapshot.json",
                &crate::log_export::sanitize_json_export(&bytes),
            ),
            Err(error) => {
                tracing::warn!(error = %error, "daemon log export unavailable");
                zip.add(
                    "daemon/snapshot.json",
                    format!("{{\"error\":{}}}\n", Value::from(error.to_string())).as_bytes(),
                );
            }
        }

        // 日志里会出现请求 URL、代理与 Webhook 错误等：所有文本日志按共享规则脱敏后再打包。
        let agent_logs = crate::log_export::agent_log_dir(self.store.data_dir());
        for (name, bytes) in crate::log_export::collect_log_files(&agent_logs).await {
            zip.add(
                &format!("agent/logs/{name}"),
                &crate::log_export::sanitize_text(&bytes),
            );
        }
        if let Some(dir) = daemon_log_dir(describe.as_ref()) {
            for (name, bytes) in crate::log_export::collect_log_files(Path::new(dir)).await {
                zip.add(
                    &format!("daemon/logs/{name}"),
                    &crate::log_export::sanitize_text(&bytes),
                );
            }
        }
        if let Some(path) = crate::log_export::nmh_relay_log_path()
            && let Some(bytes) = crate::log_export::read_tail(&path, NMH_LOG_EXPORT_BYTES).await
        {
            zip.add(
                "nmh/fluxdown_nmh.log",
                &crate::log_export::sanitize_text(&bytes),
            );
        }

        let bytes = zip.finish();
        Ok(crate::log_export::write_atomic(&target, &bytes).await?)
    }

    async fn daemon_call(&self, method: &str) -> Result<Value, DiagnosticsError> {
        self.daemon
            .call::<Value, Value>(method, None)
            .await
            .map_err(DiagnosticsError::Daemon)
    }

    async fn daemon_describe(&self) -> Option<Value> {
        tokio::time::timeout(
            DAEMON_TIMEOUT,
            self.daemon
                .call::<Value, Value>(fluxdown_protocol::method::DAEMON_DIAGNOSTICS_DESCRIBE, None),
        )
        .await
        .ok()
        .and_then(Result::ok)
    }

    async fn probe_daemon(&self) -> DaemonProbe {
        let endpoint = self.daemon_config.endpoint_label();
        let ping = tokio::time::timeout(
            DAEMON_TIMEOUT,
            self.daemon
                .call::<Value, Value>(fluxdown_protocol::method::SYSTEM_PING, None),
        )
        .await;
        match ping {
            Ok(Ok(_)) => {
                let mut detail = format!("{endpoint} → pong");
                if let Some(describe) = self.daemon_describe().await {
                    let field = |key: &str| describe.get(key).and_then(Value::as_u64).unwrap_or(0);
                    let version = describe
                        .pointer("/service/serviceVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    detail.push_str(&format!(
                        " (v{version}; tasks={}, queues={}, configRevision={})",
                        field("tasks"),
                        field("queues"),
                        field("configRevision")
                    ));
                }
                DaemonProbe {
                    connected: true,
                    check: check(CHECK_DAEMON, "", DiagnosticLevel::Ok, detail, "", None),
                }
            }
            Ok(Err(error)) => DaemonProbe {
                connected: false,
                check: check(
                    CHECK_DAEMON,
                    "",
                    DiagnosticLevel::Error,
                    format!("{endpoint} — {:?}", error.code),
                    HINT_RESTART_APP,
                    None,
                ),
            },
            Err(_) => DaemonProbe {
                connected: false,
                check: check(
                    CHECK_DAEMON,
                    "",
                    DiagnosticLevel::Error,
                    format!("{endpoint} — ping timed out"),
                    HINT_RESTART_APP,
                    None,
                ),
            },
        }
    }

    /// daemon 启动检查：崩溃循环状态来自监管器，摘要来自 `fluxdownd.stderr.log` 末尾。
    async fn probe_daemon_startup(&self) -> Option<DiagnosticCheckDto> {
        let startup = self.startup.as_ref()?;
        let streak = startup.supervisor.crash_streak();
        let looping = startup.supervisor.in_crash_loop();
        let stderr = if looping {
            crate::log_export::read_tail(&startup.stderr_log, STARTUP_STDERR_TAIL_BYTES)
                .await
                .map(|bytes| {
                    String::from_utf8_lossy(&crate::log_export::sanitize_text(&bytes)).into_owned()
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        Some(daemon_startup_check(streak, looping, &stderr))
    }

    /// `daemon.diagnostics.probe`：daemon 收集自己的目录，agent 追加分类保存目录。
    async fn probe_permissions(
        &self,
        extra_dirs: Vec<StorageProbeTarget>,
    ) -> Result<DiagnosticsProbeResult, String> {
        let params = DiagnosticsProbeParams { extra_dirs };
        match tokio::time::timeout(
            PERMISSION_PROBE_TIMEOUT,
            self.daemon
                .call::<DiagnosticsProbeParams, DiagnosticsProbeResult>(
                    fluxdown_protocol::method::DAEMON_DIAGNOSTICS_PROBE,
                    Some(params),
                ),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(format!("{:?}", error.code)),
            Err(_) => Err(format!(
                "no response within {}s",
                PERMISSION_PROBE_TIMEOUT.as_secs()
            )),
        }
    }

    /// 开机自启、系统通知与是否以提权身份运行（只读注册表 / 文件 / 会话总线 / 令牌，放进阻塞线程）。
    async fn probe_desktop(&self) -> Vec<DiagnosticCheckDto> {
        if self.desktop.is_none() {
            return Vec::new();
        }
        let notify_enabled = self
            .events
            .inspect(crate::background_effects::notify_on_complete);
        tokio::task::spawn_blocking(move || {
            let mut checks = Vec::with_capacity(3);
            #[cfg(unix)]
            if crate::permission::effective_uid().is_ok_and(|uid| uid == 0) {
                checks.push(elevated_run_check());
            }
            #[cfg(windows)]
            match crate::platform::elevation::running_elevated() {
                Ok(true) => checks.push(elevated_run_check()),
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "could not read UAC elevation for the doctor"),
            }
            checks.push(autostart_check(crate::platform::autostart_state()));
            checks.push(notification_check(
                notify_enabled.then(crate::notification::availability),
            ));
            checks
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(error = %error, "desktop doctor probes failed");
            Vec::new()
        })
    }

    /// 桌面宿主：像浏览器那样拉起已注册的 NMH 入口（本进程 IPC 端点在线才有意义；headless
    /// 不跑 NMH 服务）。注册缺失 / 失效时由中继检查报告，这里不重复。
    async fn probe_nmh_launch(
        &self,
        diagnosis: &crate::nmh::registry::NmhDiagnosis,
    ) -> Option<DiagnosticCheckDto> {
        self.desktop.as_ref()?;
        let result = crate::nmh::registry::probe_browser_launch(diagnosis).await?;
        Some(nmh_launch_check(diagnosis, &result))
    }

    /// 按界面语言发送一条测试通知；投递失败（系统拒收 / 无通知服务）作为修复失败返回。
    async fn send_test_notification(&self) -> Result<(), DiagnosticsError> {
        let desktop = self
            .desktop
            .as_ref()
            .ok_or_else(|| DiagnosticsError::InvalidAction(ACTION_TEST_NOTIFICATION.to_owned()))?;
        let locale = self
            .events
            .inspect(crate::background_effects::locale_preference);
        let title = desktop
            .text
            .text("doctorTestNotificationTitle", locale.as_deref());
        let body = desktop
            .text
            .text("doctorTestNotificationBody", locale.as_deref());
        let notifier = Arc::clone(&desktop.notifier);
        tokio::task::spawn_blocking(move || notifier.try_show(&title, &body))
            .await
            .map_err(join_error)?
            .map_err(DiagnosticsError::Notification)
    }

    /// 与 `agent.gateway.patch` 同一条路径：持久化 → 运行时开关 → 广播 `GatewayChanged`。
    async fn enable_service(&self) -> Result<(), DiagnosticsError> {
        let mut state = self.state.lock().await;
        let api_was_enabled = state.gateway.api_enabled;
        let mcp_was_enabled = state.gateway.mcp_enabled;
        state.gateway.api_enabled = true;
        crate::gateway::ensure_forced_auth_token(&mut state, api_was_enabled, mcp_was_enabled);
        let gateway = state.gateway.clone();
        let user_token = state.gateway_user_token.clone();
        self.store.save(&state).await?;
        drop(state);
        self.api_switches.update(
            gateway.takeover_enabled,
            gateway.jsonrpc_enabled,
            gateway.api_enabled,
            gateway.mcp_enabled,
            gateway.cors_enabled,
        );
        self.api_token.set(user_token);
        self.events
            .publish(fluxdown_protocol::AgentEvent::GatewayChanged(gateway));
        Ok(())
    }

    /// 不含 token、凭据与设备私钥的 agent 状态摘要。
    async fn agent_summary(&self) -> Value {
        let state = self.state.lock().await;
        json!({
            "appVersion": fluxdown_protocol::APP_VERSION,
            "platform": format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            "dataDir": self.store.data_dir().display().to_string(),
            "daemonRpcUrl": self.daemon_config.endpoint_label(),
            "deviceId": state.device_id,
            "deviceName": state.device_name,
            "devicePlatform": state.platform,
            "signedIn": state.credentials.is_some(),
            "gateway": state.gateway,
            "sync": state.sync,
            "preferences": state.preferences,
            "linkedDevices": state.linked_devices.len(),
            "remoteTasks": state.remote_tasks.len(),
            "ipcEndpoint": crate::nmh::ipc_endpoint(),
        })
    }

    /// `daemon.diagnostics.prepareLogExport` → `GET /exports/{id}`（一次性 blob）。
    async fn fetch_daemon_export(&self) -> Result<Vec<u8>, DiagnosticsError> {
        let prepared = tokio::time::timeout(
            DAEMON_TIMEOUT,
            self.daemon.call::<Value, Value>(
                fluxdown_protocol::method::DAEMON_DIAGNOSTICS_PREPARE_LOG_EXPORT,
                None,
            ),
        )
        .await
        .map_err(|_| DiagnosticsError::Export("prepareLogExport timed out".to_owned()))?
        .map_err(DiagnosticsError::Daemon)?;
        let export_id = prepared
            .get("exportId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                DiagnosticsError::Export("prepareLogExport returned no exportId".to_owned())
            })?;
        // 会话级凭据与地址只在已认证的 daemon 连接上存在；没有会话就是 daemon 不可用，不退回
        // 长期 token。
        let access = self.daemon_config.http_session().access().ok_or_else(|| {
            DiagnosticsError::Export("daemon session is not established".to_owned())
        })?;
        let url = daemon_export_url(&access.base_url, export_id);
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(DAEMON_TIMEOUT)
            .build()
            .map_err(|error| DiagnosticsError::Export(error.to_string()))?;
        let credential = access.credential;
        let response = client
            .get(url)
            .bearer_auth(credential)
            .send()
            .await
            .map_err(|error| DiagnosticsError::Export(error.to_string()))?;
        if !response.status().is_success() {
            return Err(DiagnosticsError::Export(format!(
                "daemon export returned {}",
                response.status()
            )));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| DiagnosticsError::Export(error.to_string()))
    }
}

struct DaemonProbe {
    connected: bool,
    check: DiagnosticCheckDto,
}

/// 无需 `await` 的探测（文件、注册表、平台集成状态），在一次 `spawn_blocking` 内完成。
struct SyncProbe {
    nmh: Vec<DiagnosticCheckDto>,
    /// 供随后的浏览器式拉起探测复用，避免再读一遍注册。
    nmh_diagnosis: crate::nmh::registry::NmhDiagnosis,
    shell: Vec<DiagnosticCheckDto>,
    log_dir: DiagnosticCheckDto,
}

fn probe_sync(data_dir: &Path, opted_out: &[OpenAssociation]) -> SyncProbe {
    let nmh_diagnosis = crate::nmh::registry::diagnose();
    SyncProbe {
        nmh: nmh_checks(&nmh_diagnosis),
        nmh_diagnosis,
        shell: shell_checks(&crate::platform::integration_status(), opted_out),
        log_dir: probe_log_dir(&crate::log_export::agent_log_dir(data_dir)),
    }
}

/// 崩溃循环检查详情里保留的 daemon stderr 末尾行数。
const STARTUP_STDERR_LINES: usize = 8;
/// 读取 stderr 日志末尾的字节数。
const STARTUP_STDERR_TAIL_BYTES: u64 = 4 * 1024;

/// `daemon_startup`：daemon 子进程是否在「启动即崩溃」的循环里（端口无法监听、引擎初始化
/// 失败等）。循环中时详情带 `fluxdownd.stderr.log` 的末尾摘要（已脱敏）。
fn daemon_startup_check(
    crash_streak: u32,
    in_crash_loop: bool,
    stderr: &str,
) -> DiagnosticCheckDto {
    if !in_crash_loop {
        return check(
            CHECK_DAEMON_STARTUP,
            "",
            DiagnosticLevel::Ok,
            "fluxdownd is not crash-looping".to_owned(),
            "",
            None,
        );
    }
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(STARTUP_STDERR_LINES)..];
    let mut detail = format!(
        "fluxdownd exited abnormally {crash_streak} times in a row right after launch \
         (local port unavailable or startup failure); see fluxdownd.stderr.log"
    );
    for line in tail {
        detail.push('\n');
        detail.push_str(line);
    }
    check(
        CHECK_DAEMON_STARTUP,
        "",
        DiagnosticLevel::Error,
        detail,
        HINT_DAEMON_STARTUP_FAILED,
        None,
    )
}

/// 无 RSS 间隔（`0`）时引擎使用的默认抓取间隔（分钟），与 engine `DEFAULT_INTERVAL_MINUTES` 一致。
const RSS_DEFAULT_INTERVAL_MINUTES: i64 = 30;

/// 会在空闲时唤醒硬盘的来源汇总；全部取自 agent 已缓存的 daemon 快照，探测本身零磁盘访问。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiskSleepBlockers {
    /// `seeding_status == 1`（做种中）的任务数；排队等待做种槽的不计。
    seeding_tasks: usize,
    /// 启用的 RSS 源数量。
    rss_sources: usize,
    /// 启用的 RSS 源里最短的配置抓取间隔（分钟）。
    shortest_rss_minutes: Option<i64>,
    /// 空闲时是否仍周期性扫描已完成任务的文件。
    idle_file_scan: bool,
}

impl DiskSleepBlockers {
    fn any(&self) -> bool {
        self.seeding_tasks > 0 || self.rss_sources > 0 || self.idle_file_scan
    }
}

/// 从 agent 快照收集阻碍项；daemon 未连接时快照里的任务/RSS 已过期，不产出该检查。
fn disk_sleep_blockers(snapshot: &AgentSnapshot) -> Option<DiskSleepBlockers> {
    if !snapshot.daemon_connected {
        return None;
    }
    let daemon = &snapshot.daemon;
    let enabled_intervals = daemon
        .rss_sources
        .iter()
        .filter(|source| source.enabled)
        .map(|source| match i64::from(source.interval_minutes) {
            minutes if minutes > 0 => minutes,
            _ => RSS_DEFAULT_INTERVAL_MINUTES,
        });
    let idle_file_scan = daemon
        .config
        .values
        .get("idle_file_scan")
        .map_or_else(
            || fluxdown_protocol::daemon_config_default("idle_file_scan"),
            String::as_str,
        )
        .trim()
        .to_ascii_lowercase();
    Some(DiskSleepBlockers {
        seeding_tasks: daemon
            .tasks
            .iter()
            .filter(|task| task.seeding_status == 1)
            .count(),
        rss_sources: enabled_intervals.clone().count(),
        shortest_rss_minutes: enabled_intervals.min(),
        idle_file_scan: matches!(idle_file_scan.as_str(), "true" | "1"),
    })
}

/// `disk_sleep`：列出会让 NAS 硬盘无法休眠的来源。这些都是用户主动启用的功能而非故障，
/// 所以有阻碍项时只报 info（不计入「需要关注」），无阻碍项报 ok。
fn disk_sleep_check(blockers: &DiskSleepBlockers) -> DiagnosticCheckDto {
    if !blockers.any() {
        return check(
            CHECK_DISK_SLEEP,
            "",
            DiagnosticLevel::Ok,
            "No seeding tasks, enabled RSS sources or idle file scan: \
             the daemon does not touch the disk while idle"
                .to_owned(),
            "",
            None,
        );
    }
    let mut items = Vec::with_capacity(3);
    if blockers.seeding_tasks > 0 {
        items.push(format!("seeding tasks: {}", blockers.seeding_tasks));
    }
    if blockers.rss_sources > 0 {
        let mut item = format!("enabled RSS sources: {}", blockers.rss_sources);
        if let Some(minutes) = blockers.shortest_rss_minutes {
            item.push_str(&format!(" (shortest interval {minutes} min)"));
        }
        items.push(item);
    }
    if blockers.idle_file_scan {
        items.push("idle file scan: on".to_owned());
    }
    check(
        CHECK_DISK_SLEEP,
        "",
        DiagnosticLevel::Info,
        format!(
            "Active while idle and may keep the disk awake — {}",
            items.join("; ")
        ),
        HINT_DISK_SLEEP_BLOCKERS,
        None,
    )
}

/// 分类保存目录（agent 偏好里只有 agent 知道的目录）：可见且配置了目录的分类。
fn category_probe_targets(snapshot: &AgentSnapshot) -> Vec<StorageProbeTarget> {
    CustomCategoryDto::from_preference(snapshot.preferences.values.get(CUSTOM_CATEGORIES_PREF_KEY))
        .into_iter()
        .filter(|category| category.visible && !category.save_dir.trim().is_empty())
        .map(|category| StorageProbeTarget {
            role: StorageProbeRole::Category,
            label: if category.name.trim().is_empty() {
                category.builtin_type.unwrap_or(category.id)
            } else {
                category.name
            },
            path: category.save_dir,
        })
        .collect()
}

/// daemon 动态探测结果 → 检查项：目录按 daemon 返回顺序，组件在后。`desktop` 决定是否提供
/// 需要桌面会话（授权对话框 / 系统设置页）的修复按钮。
fn permission_checks(
    result: &DiagnosticsProbeResult,
    desktop: bool,
    os: crate::permission::Os,
) -> Vec<DiagnosticCheckDto> {
    result
        .storage
        .iter()
        .map(|probe| storage_check(probe, desktop, os))
        .chain(
            result
                .components
                .iter()
                .map(|probe| component_check(probe, os)),
        )
        .collect()
}

/// 单个目录的写入探测：失败报错并按失败类型给建议；通过但剩余空间低于
/// [`LOW_SPACE_BYTES`] 报警告。
fn storage_check(
    probe: &StorageProbeDto,
    desktop: bool,
    os: crate::permission::Os,
) -> DiagnosticCheckDto {
    let id = match probe.role {
        StorageProbeRole::DefaultSaveDir => CHECK_SAVE_DIR,
        StorageProbeRole::Queue => CHECK_QUEUE_SAVE_DIR,
        StorageProbeRole::Rss => CHECK_RSS_SAVE_DIR,
        StorageProbeRole::Category => CHECK_CATEGORY_SAVE_DIR,
        StorageProbeRole::DataDir => CHECK_DATA_DIR,
    };
    let mut detail = probe.path.clone();
    if !probe.exists && !probe.probed_dir.is_empty() {
        let state = if probe.failure.is_none() {
            "not created yet (created on first download)"
        } else {
            "does not exist"
        };
        detail.push_str(&format!("\n{state}; probed {}", probe.probed_dir));
    }
    if let Some(bytes) = probe.available_bytes {
        detail.push_str(&format!("\nfree space: {}", format_bytes(bytes)));
    }
    let (level, hint, repair) = match probe.failure {
        Some(failure) => {
            detail.push('\n');
            if !probe.failed_step.is_empty() {
                detail.push_str(&format!("{} failed: ", probe.failed_step));
            }
            detail.push_str(&probe.error);
            (
                DiagnosticLevel::Error,
                storage_failure_hint(failure, os, probe.os_error),
                storage_repair(probe, desktop, os),
            )
        }
        None if probe
            .available_bytes
            .is_some_and(|bytes| bytes < LOW_SPACE_BYTES) =>
        {
            (DiagnosticLevel::Warn, HINT_LOW_DISK_SPACE, None)
        }
        None => (DiagnosticLevel::Ok, "", None),
    };
    check(
        id,
        &probe.label,
        level,
        detail,
        hint,
        repair
            .as_ref()
            .map(|(action, target)| (*action, target.as_str())),
    )
}

/// macOS 上 `EPERM` 是隐私授权（TCC）拒绝，`EACCES` 才是权限位 / ACL。
fn storage_failure_hint(
    failure: StorageProbeFailure,
    os: crate::permission::Os,
    os_error: Option<i32>,
) -> &'static str {
    match failure {
        StorageProbeFailure::PermissionDenied
            if os == crate::permission::Os::MacOs && os_error == Some(crate::permission::EPERM) =>
        {
            HINT_DIR_PERMISSION_MACOS
        }
        StorageProbeFailure::PermissionDenied => HINT_DIR_PERMISSION,
        StorageProbeFailure::ReadOnly => HINT_DIR_READ_ONLY,
        StorageProbeFailure::NoSpace => HINT_LOW_DISK_SPACE,
        StorageProbeFailure::Missing => HINT_DIR_MISSING,
        StorageProbeFailure::Timeout => HINT_DIR_TIMEOUT,
        StorageProbeFailure::NotDirectory | StorageProbeFailure::Other => HINT_CHECK_DISK,
    }
}

/// 写入被拒时的修复按钮：权限位 / ACL 问题给「修复权限」，macOS 隐私授权给「打开系统设置」；
/// 两者都需要桌面会话。其它失败（只读、满盘、不可变属性）没有自动修复。
fn storage_repair(
    probe: &StorageProbeDto,
    desktop: bool,
    os: crate::permission::Os,
) -> Option<(&'static str, String)> {
    use crate::permission::{EACCES, EPERM, ERROR_ACCESS_DENIED, Os};
    if !desktop || probe.failure != Some(StorageProbeFailure::PermissionDenied) {
        return None;
    }
    match (os, probe.os_error) {
        (Os::MacOs, Some(EPERM)) => Some((
            ACTION_OPEN_SETTINGS,
            crate::platform::SettingsPane::FilesAndFolders
                .name()
                .to_owned(),
        )),
        (Os::Linux | Os::MacOs, Some(EACCES)) | (Os::Windows, Some(ERROR_ACCESS_DENIED)) => {
            Some((ACTION_FIX_DIR_ACCESS, probe.path.clone()))
        }
        _ => None,
    }
}

/// 外部组件实际运行结果；启动被拒与运行失败给不同建议。托管组件（daemon 数据目录内）在
/// 类 Unix 上缺执行位时可就地修复（由 daemon 执行，不需要桌面会话）。
fn component_check(probe: &ComponentProbeDto, os: crate::permission::Os) -> DiagnosticCheckDto {
    if probe.error.is_empty() {
        return check(
            CHECK_COMPONENT,
            &probe.name,
            DiagnosticLevel::Ok,
            format!("{} → {}", probe.path, probe.output),
            "",
            None,
        );
    }
    let fixable = probe.permission_denied && probe.managed && os != crate::permission::Os::Windows;
    check(
        CHECK_COMPONENT,
        &probe.name,
        DiagnosticLevel::Error,
        format!("{} — {}", probe.path, probe.error),
        match (probe.permission_denied, fixable) {
            (true, true) => HINT_COMPONENT_BLOCKED_FIXABLE,
            (true, false) => HINT_COMPONENT_BLOCKED,
            (false, _) => HINT_COMPONENT_BROKEN,
        },
        fixable.then_some((ACTION_FIX_COMPONENT, probe.name.as_str())),
    )
}

/// 开机自启：用户没开是选择而非故障；开了却被系统级开关禁用才需要关注，并可一键重新启用
/// （用户在 Doctor 里的明确操作，等同于在设置里重新打开）。
fn autostart_check(state: crate::platform::AutostartState) -> DiagnosticCheckDto {
    use crate::platform::AutostartState;
    let (level, detail, hint, repair) = match state {
        AutostartState::Unsupported => (
            DiagnosticLevel::Info,
            "not supported by this installation",
            "",
            None,
        ),
        AutostartState::Off => (DiagnosticLevel::Ok, "turned off in settings", "", None),
        AutostartState::Enabled => (DiagnosticLevel::Ok, "enabled", "", None),
        AutostartState::DisabledBySystem => (
            DiagnosticLevel::Warn,
            "turned on in FluxDown but disabled by the system startup settings",
            HINT_AUTOSTART_BLOCKED,
            Some((ACTION_ENABLE_AUTOSTART, "")),
        ),
    };
    check(CHECK_AUTOSTART, "", level, detail.to_owned(), hint, repair)
}

/// 系统通知：`None` = 完成通知已在设置中关闭，不探测。被系统关闭时优先打开通知设置页（平台
/// 有该页时），其余能投递的情况附「发送测试通知」。
fn notification_check(
    availability: Option<crate::notification::NotificationAvailability>,
) -> DiagnosticCheckDto {
    use crate::notification::NotificationAvailability;
    use crate::platform::SettingsPane;
    let test = Some((ACTION_TEST_NOTIFICATION, ""));
    let (level, detail, hint, repair) = match availability {
        None => (
            DiagnosticLevel::Ok,
            "download notifications are turned off in settings".to_owned(),
            "",
            None,
        ),
        Some(NotificationAvailability::Available(detail)) => {
            (DiagnosticLevel::Ok, detail, "", test)
        }
        Some(NotificationAvailability::Blocked(detail)) => (
            DiagnosticLevel::Warn,
            detail,
            HINT_NOTIFICATIONS_BLOCKED,
            if SettingsPane::Notifications.uri().is_some() {
                Some((ACTION_OPEN_SETTINGS, SettingsPane::Notifications.name()))
            } else {
                test
            },
        ),
        Some(NotificationAvailability::Unavailable(detail)) => (
            DiagnosticLevel::Warn,
            format!("no notification service: {detail}"),
            HINT_NOTIFICATIONS_UNAVAILABLE,
            None,
        ),
        Some(NotificationAvailability::Unverifiable) => (
            DiagnosticLevel::Info,
            "notification permission is not yet determined or cannot be verified".to_owned(),
            HINT_NOTIFICATIONS_UNVERIFIED,
            test,
        ),
    };
    check(CHECK_NOTIFICATIONS, "", level, detail, hint, repair)
}

/// 以提权身份运行的桌面宿主。类 Unix root：写出的文件都属于 root，回到普通身份后改不动；
/// Windows 完整管理员令牌：启动时降权失败才会出现（见 `platform::elevation`）。
#[cfg(any(unix, windows))]
fn elevated_run_check() -> DiagnosticCheckDto {
    #[cfg(unix)]
    let (detail, hint) = ("fluxdown-agent is running as root", HINT_RUNNING_ELEVATED);
    #[cfg(windows)]
    let (detail, hint) = (
        "fluxdown-agent is running elevated (administrator token)",
        HINT_RUNNING_AS_ADMIN,
    );
    check(
        CHECK_ELEVATED_RUN,
        "",
        DiagnosticLevel::Warn,
        detail.to_owned(),
        hint,
        None,
    )
}

/// 浏览器式拉起 NMH 入口的结果；失败时「重新注册」会重写注册并在需要时请求授权释放旧文件。
fn nmh_launch_check(
    diagnosis: &crate::nmh::registry::NmhDiagnosis,
    result: &Result<(), crate::nmh::registry::RelayLaunchError>,
) -> DiagnosticCheckDto {
    let entry = diagnosis.launch_entry();
    match result {
        Ok(()) => check(
            CHECK_NMH_LAUNCH,
            "",
            DiagnosticLevel::Ok,
            format!("{entry} → pong"),
            "",
            None,
        ),
        Err(error) => check(
            CHECK_NMH_LAUNCH,
            "",
            DiagnosticLevel::Error,
            format!("{entry} — {error}"),
            HINT_NMH_LAUNCH_FAILED,
            Some((ACTION_REREGISTER, "")),
        ),
    }
}

/// 1024 进制、一位小数（`12.3 GB`）。
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn check(
    id: &str,
    target: &str,
    level: DiagnosticLevel,
    detail: String,
    hint: &str,
    repair: Option<(&str, &str)>,
) -> DiagnosticCheckDto {
    DiagnosticCheckDto {
        id: id.to_owned(),
        target: target.to_owned(),
        level,
        detail,
        hint: hint.to_owned(),
        repair: repair.map(|(action, target)| DiagnosticRepairParams {
            action: action.to_owned(),
            target: target.to_owned(),
        }),
    }
}

/// 清单检查要求存在的浏览器家族（Chromium 系、Firefox 系）。
///
/// 仅 Unix 按安装状态过滤（#713）：Windows 的两份清单由注册无条件写入，而安装探测只覆盖默认
/// 目录，自定义 `--user-data-dir` 或经 Chrome 注册表键工作的 Brave / Vivaldi 会被漏判，
/// 因此 Windows 保持「缺失即错误」。
fn manifest_families_installed(diagnosis: &crate::nmh::registry::NmhDiagnosis) -> (bool, bool) {
    if cfg!(windows) {
        return (true, true);
    }
    if diagnosis.targets.is_empty() {
        return (
            !diagnosis.chromium_manifest.is_empty(),
            !diagnosis.firefox_manifest.is_empty(),
        );
    }
    (
        diagnosis
            .targets
            .iter()
            .any(|t| !is_firefox_target(&t.label) && t.installed),
        diagnosis
            .targets
            .iter()
            .any(|t| is_firefox_target(&t.label) && t.installed),
    )
}

/// `nmh_binary`、`nmh_manifest`、`nmh_relay`、每个浏览器一条 `nmh_browser`。
fn nmh_checks(diagnosis: &crate::nmh::registry::NmhDiagnosis) -> Vec<DiagnosticCheckDto> {
    let mut checks = Vec::with_capacity(3 + diagnosis.targets.len());
    if diagnosis.exe_path.is_empty() {
        checks.push(check(
            CHECK_NMH_BINARY,
            "",
            DiagnosticLevel::Error,
            diagnosis.exe_error.clone(),
            HINT_REINSTALL_APP,
            None,
        ));
    } else {
        checks.push(check(
            CHECK_NMH_BINARY,
            "",
            DiagnosticLevel::Ok,
            diagnosis.exe_path.clone(),
            "",
            None,
        ));
    }
    let (chromium_installed, firefox_installed) = manifest_families_installed(diagnosis);
    checks.push(manifest_check(
        &diagnosis.chromium_manifest,
        chromium_installed,
        &diagnosis.firefox_manifest,
        firefox_installed,
    ));
    if !diagnosis.exe_path.is_empty() {
        checks.push(relay_check(diagnosis));
    }
    for target in &diagnosis.targets {
        let (level, detail, hint, repair) = if !target.installed {
            (
                DiagnosticLevel::Info,
                format!("{} (browser not installed)", target.location),
                "",
                None,
            )
        } else if target.ok {
            (DiagnosticLevel::Ok, target.location.clone(), "", None)
        } else {
            (
                DiagnosticLevel::Error,
                format!("{} — {}", target.location, target.issue),
                HINT_REREGISTER_NMH,
                Some((ACTION_REREGISTER, "")),
            )
        };
        checks.push(check(
            CHECK_NMH_BROWSER,
            &target.label,
            level,
            detail,
            hint,
            repair,
        ));
    }
    if !diagnosis.foreign_owned.is_empty() {
        checks.push(check(
            CHECK_NMH_OWNERSHIP,
            "",
            DiagnosticLevel::Warn,
            format!(
                "owned by another user:\n{}",
                diagnosis.foreign_owned.join("\n")
            ),
            HINT_NMH_FOREIGN_OWNED,
            Some((ACTION_REREGISTER, "")),
        ));
    }
    checks.extend(diagnosis.comspec.as_ref().map(comspec_check));
    checks.extend(diagnosis.policy_blocks.iter().map(policy_check));
    checks
}

/// `ComSpec` 以注册表配置值（新启动的程序拿到的值）为准；本进程环境只用来发现「已修正但
/// 修改前启动的程序仍沿用旧值」——Windows 不会把环境变量的修改推给已在运行的进程。
fn comspec_check(state: &crate::nmh::comspec::ComSpecState) -> DiagnosticCheckDto {
    use crate::nmh::comspec::DEFAULT_COMSPEC;

    let configured = state.configured.as_ref().map(|configured| {
        let value = if configured.raw == configured.path {
            format!("\"{}\"", configured.raw)
        } else {
            format!("\"{}\" → {}", configured.raw, configured.path)
        };
        format!("{value} ({})", configured.source.location())
    });
    if let Some(issue) = state.configured_issue {
        let current =
            configured.unwrap_or_else(|| "neither the user nor the system variable".to_owned());
        return check(
            CHECK_NMH_COMSPEC,
            "",
            DiagnosticLevel::Error,
            format!("ComSpec = {current}: {issue}\nexpected: {DEFAULT_COMSPEC}"),
            HINT_NMH_COMSPEC_INVALID,
            Some((ACTION_FIX_COMSPEC, "")),
        );
    }
    let configured = configured.unwrap_or_default();
    if let Some(issue) = state.process_issue {
        let process = state
            .process
            .as_deref()
            .map_or_else(|| "(not set)".to_owned(), |value| format!("\"{value}\""));
        return check(
            CHECK_NMH_COMSPEC,
            "",
            DiagnosticLevel::Warn,
            format!(
                "programs started from now on: {configured}\nprograms started before the change, FluxDown included: {process}: {issue}"
            ),
            HINT_NMH_COMSPEC_RESTART,
            Some((ACTION_FIX_COMSPEC, "")),
        );
    }
    check(
        CHECK_NMH_COMSPEC,
        "",
        DiagnosticLevel::Ok,
        configured,
        "",
        None,
    )
}

/// 组织策略是管理员的明确意图：只报告，不提供修复（FluxDown 不绕过策略）。
fn policy_check(block: &crate::nmh::registry::PolicyBlock) -> DiagnosticCheckDto {
    use crate::nmh::registry::PolicyBlock;
    let (target, detail, hint) = match block {
        PolicyBlock::UserLevelHostsDisabled { browser } => (
            browser.as_str(),
            "NativeMessagingUserLevelHosts = 0: per-user native messaging hosts are ignored",
            HINT_NMH_USER_HOSTS_DISABLED,
        ),
        PolicyBlock::Blocklisted { browser } => (
            browser.as_str(),
            "NativeMessagingBlocklist blocks com.fluxdown.nmh",
            HINT_NMH_POLICY_BLOCKED,
        ),
        PolicyBlock::CommandPromptDisabled => (
            "",
            "DisableCMD = 1: Chrome / Edge cannot start native messaging hosts",
            HINT_NMH_CMD_DISABLED,
        ),
    };
    check(
        CHECK_NMH_POLICY,
        target,
        DiagnosticLevel::Error,
        detail.to_owned(),
        hint,
        None,
    )
}

/// 注册入口（启动脚本 / 注册表键）实际指向哪个中继。这是所有浏览器共用的根因，只报一次；
/// 指向另一份仍可用的 FluxDown 安装只是提示，扩展照常工作。
fn relay_check(diagnosis: &crate::nmh::registry::NmhDiagnosis) -> DiagnosticCheckDto {
    use crate::nmh::registry::RelayOwner;

    let location = &diagnosis.relay_location;
    let relay = &diagnosis.registered_relay;
    let (level, detail, hint, action) = match diagnosis.relay_owner {
        RelayOwner::Current => (
            DiagnosticLevel::Ok,
            format!("{location} → {relay}"),
            "",
            None,
        ),
        RelayOwner::OtherInstall => (
            DiagnosticLevel::Info,
            format!("{location} → {relay} (another FluxDown installation)"),
            HINT_NMH_OTHER_INSTALL,
            Some(ACTION_USE_THIS_INSTALL),
        ),
        RelayOwner::Broken if relay.is_empty() => (
            DiagnosticLevel::Error,
            format!("{location} — cannot resolve relay path"),
            HINT_REREGISTER_NMH,
            Some(ACTION_REREGISTER),
        ),
        RelayOwner::Broken => (
            DiagnosticLevel::Error,
            format!("{location} → {relay} (relay missing or not executable)"),
            HINT_REREGISTER_NMH,
            Some(ACTION_REREGISTER),
        ),
        RelayOwner::Missing => (
            DiagnosticLevel::Error,
            format!("{location} — not registered"),
            HINT_REREGISTER_NMH,
            Some(ACTION_REREGISTER),
        ),
    };
    check(
        CHECK_NMH_RELAY,
        "",
        level,
        detail,
        hint,
        action.map(|action| (action, "")),
    )
}

/// 判断 NMH 目标是否属于 Firefox 家族。
/// 注意：前缀匹配必须与 `nmh.rs` 中的 `label_for_dir` 保持同步。
fn is_firefox_target(label: &str) -> bool {
    label.starts_with("Firefox")
        || label.starts_with("LibreWolf")
        || label.starts_with("Zen")
        || label.starts_with("Mozilla")
}

/// 已安装浏览器家族的清单都应存在；未安装的浏览器家族清单不作为缺失处理（#713）。
fn manifest_check(
    chromium: &str,
    chromium_installed: bool,
    firefox: &str,
    firefox_installed: bool,
) -> DiagnosticCheckDto {
    let mut missing = Vec::new();
    if chromium_installed && (chromium.is_empty() || !Path::new(chromium).exists()) {
        missing.push("chromium");
    }
    if firefox_installed && (firefox.is_empty() || !Path::new(firefox).exists()) {
        missing.push("firefox");
    }

    let detail = format!(
        "chromium: {}\nfirefox: {}",
        if chromium_installed {
            chromium
        } else {
            "(not installed)"
        },
        if firefox_installed {
            firefox
        } else {
            "(not installed)"
        },
    );

    if !chromium_installed && !firefox_installed {
        check(
            CHECK_NMH_MANIFEST,
            "",
            DiagnosticLevel::Info,
            detail,
            "",
            None,
        )
    } else if missing.is_empty() {
        check(
            CHECK_NMH_MANIFEST,
            "",
            DiagnosticLevel::Ok,
            detail,
            "",
            None,
        )
    } else {
        check(
            CHECK_NMH_MANIFEST,
            "",
            DiagnosticLevel::Error,
            format!("{detail}\nmissing: {}", missing.join(", ")),
            HINT_REREGISTER_NMH,
            Some((ACTION_REREGISTER, "")),
        )
    }
}

/// 用户在设置中手动关闭的关联（`OpenAssociation::opt_out_pref_key` 为 `true`）。
fn opted_out_associations(snapshot: &AgentSnapshot) -> Vec<OpenAssociation> {
    [
        OpenAssociation::Torrent,
        OpenAssociation::Magnet,
        OpenAssociation::Ed2k,
    ]
    .into_iter()
    .filter(|association| {
        snapshot
            .preferences
            .values
            .get(association.opt_out_pref_key())
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
    .collect()
}

/// 用户已关闭的关联：按意图报告，不提供「立即注册」（绕过设置页会与 opt-out 偏好矛盾）。
/// 系统仍指向 FluxDown 说明没有其他候选可接手，报 info 解释 FluxDown 会忽略这些打开请求。
fn opted_out_check(id: &str, target: &str, still_registered: bool) -> DiagnosticCheckDto {
    if still_registered {
        check(
            id,
            target,
            DiagnosticLevel::Info,
            "turned off in settings; no other handler installed, FluxDown ignores these opens"
                .to_owned(),
            HINT_ASSOCIATION_OFF,
            None,
        )
    } else {
        check(
            id,
            target,
            DiagnosticLevel::Ok,
            "turned off in settings".to_owned(),
            "",
            None,
        )
    }
}

/// `url_protocol`×3 与 `torrent_association`。
///
/// `fluxdown://` 是深链入口，缺失报 warn；`magnet`/`ed2k`/`.torrent` 是可选项，只报 info，
/// 否则用户会习惯性忽略整页。用户已在设置中关闭的关联按 [`opted_out_check`] 报告。
fn shell_checks(
    integration: &PlatformIntegrationDto,
    opted_out: &[OpenAssociation],
) -> Vec<DiagnosticCheckDto> {
    let mut checks = Vec::with_capacity(URL_SCHEMES.len() + 1);
    for scheme in URL_SCHEMES {
        let registered = integration
            .url_protocols
            .get(scheme)
            .copied()
            .unwrap_or(false);
        let association = match scheme {
            "magnet" => Some(OpenAssociation::Magnet),
            "ed2k" => Some(OpenAssociation::Ed2k),
            _ => None,
        };
        if integration.url_protocol_supported
            && association.is_some_and(|association| opted_out.contains(&association))
        {
            checks.push(opted_out_check(CHECK_URL_PROTOCOL, scheme, registered));
            continue;
        }
        let (level, detail, hint, repair) = if !integration.url_protocol_supported {
            (
                DiagnosticLevel::Info,
                "not supported on this platform".to_owned(),
                "",
                None,
            )
        } else if registered {
            (
                DiagnosticLevel::Ok,
                "registered for this build".to_owned(),
                "",
                None,
            )
        } else if scheme == "fluxdown" {
            (
                DiagnosticLevel::Warn,
                "not registered".to_owned(),
                HINT_ENABLE_PROTOCOL,
                Some((ACTION_REGISTER, scheme)),
            )
        } else {
            (
                DiagnosticLevel::Info,
                "not registered".to_owned(),
                HINT_ENABLE_PROTOCOL,
                Some((ACTION_REGISTER, scheme)),
            )
        };
        checks.push(check(
            CHECK_URL_PROTOCOL,
            scheme,
            level,
            detail,
            hint,
            repair,
        ));
    }
    let torrent = if !integration.file_association_supported {
        check(
            CHECK_TORRENT_ASSOCIATION,
            "",
            DiagnosticLevel::Info,
            "not supported on this platform".to_owned(),
            "",
            None,
        )
    } else if opted_out.contains(&OpenAssociation::Torrent) {
        opted_out_check(
            CHECK_TORRENT_ASSOCIATION,
            "",
            integration.torrent_associated,
        )
    } else if integration.torrent_associated {
        check(
            CHECK_TORRENT_ASSOCIATION,
            "",
            DiagnosticLevel::Ok,
            ".torrent opens with FluxDown".to_owned(),
            "",
            None,
        )
    } else {
        check(
            CHECK_TORRENT_ASSOCIATION,
            "",
            DiagnosticLevel::Info,
            ".torrent not associated".to_owned(),
            HINT_ENABLE_PROTOCOL,
            Some((ACTION_REGISTER, TARGET_TORRENT)),
        )
    };
    checks.push(torrent);
    checks
}

/// 磁盘满或无权限的日志目录会吞掉 bug 报告需要的证据，所以真的写一次。
fn probe_log_dir(dir: &Path) -> DiagnosticCheckDto {
    let dir_str = dir.display().to_string();
    let (files, total) = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".log"))
                .filter_map(|entry| entry.metadata().ok())
                .fold((0_usize, 0_u64), |(count, bytes), metadata| {
                    (count + 1, bytes + metadata.len())
                })
        })
        .unwrap_or((0, 0));
    let summary = format!(
        "{dir_str} ({files} log files, {:.1} MB)",
        total as f64 / (1024.0 * 1024.0)
    );
    let repair = Some((ACTION_OPEN_LOG_DIR, dir_str.as_str()));
    if !dir.is_dir() {
        return check(
            CHECK_LOG_DIR,
            "",
            DiagnosticLevel::Error,
            format!("{summary} — directory missing"),
            HINT_CHECK_DISK,
            repair,
        );
    }
    let probe_file = dir.join(".doctor_write_probe");
    match std::fs::write(&probe_file, b"") {
        Ok(()) => {
            if let Err(error) = std::fs::remove_file(&probe_file)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return check(
                    CHECK_LOG_DIR,
                    "",
                    DiagnosticLevel::Error,
                    format!("{summary} — probe cleanup failed: {error}"),
                    HINT_CHECK_DISK,
                    repair,
                );
            }
            check(CHECK_LOG_DIR, "", DiagnosticLevel::Ok, summary, "", repair)
        }
        Err(error) => check(
            CHECK_LOG_DIR,
            "",
            DiagnosticLevel::Error,
            format!("{summary} — not writable: {error}"),
            HINT_CHECK_DISK,
            repair,
        ),
    }
}

/// 浏览器中继拨号的 IPC 端点是否应答 `pong`，以及网关 TCP 端口是否可连。
async fn probe_listener(port: u16) -> DiagnosticCheckDto {
    let ipc_endpoint = crate::nmh::ipc_endpoint();
    let ipc = crate::nmh::probe_ipc(PROBE_TIMEOUT).await;
    let tcp_address = format!("127.0.0.1:{port}");
    let tcp =
        match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(&tcp_address))
            .await
        {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connect timed out",
            )),
        };
    let mut detail = match &ipc {
        Ok(reply) => format!("ipc {ipc_endpoint} → {reply}"),
        Err(error) => format!("ipc {ipc_endpoint} — {error}"),
    };
    detail.push('\n');
    match &tcp {
        Ok(_) => detail.push_str(&format!("gateway {tcp_address} → connected")),
        Err(error) => detail.push_str(&format!("gateway {tcp_address} — {error}")),
    }
    if ipc.is_ok() && tcp.is_ok() {
        check(
            CHECK_APP_LISTENER,
            "",
            DiagnosticLevel::Ok,
            detail,
            "",
            None,
        )
    } else {
        check(
            CHECK_APP_LISTENER,
            "",
            DiagnosticLevel::Error,
            detail,
            HINT_RESTART_APP,
            None,
        )
    }
}

/// 兼容 HTTP API：任一功能开关打开才算启用，然后探活 `/ping`。
async fn probe_local_server(gateway: &fluxdown_protocol::GatewayStatusDto) -> DiagnosticCheckDto {
    let enabled = gateway.api_enabled
        || gateway.takeover_enabled
        || gateway.jsonrpc_enabled
        || gateway.mcp_enabled;
    if !enabled {
        return check(
            CHECK_LOCAL_SERVER,
            "",
            DiagnosticLevel::Info,
            "disabled in settings".to_owned(),
            HINT_ENABLE_LOCAL_SERVER,
            Some((ACTION_ENABLE_SERVICE, "")),
        );
    }
    let url = format!(
        "http://127.0.0.1:{}{}",
        gateway.port,
        fluxdown_api::routes::PING
    );
    // `.no_proxy()`：系统代理会吞掉回环探测，把健康的服务误报为不可达。
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return check(
                CHECK_LOCAL_SERVER,
                "",
                DiagnosticLevel::Error,
                format!("{url} — client build failed: {error}"),
                "",
                None,
            );
        }
    };
    let flags = format!(
        "api={}, takeover={}, jsonrpc={}, mcp={}, lan={}",
        gateway.api_enabled,
        gateway.takeover_enabled,
        gateway.jsonrpc_enabled,
        gateway.mcp_enabled,
        gateway.lan_enabled
    );
    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => check(
            CHECK_LOCAL_SERVER,
            "",
            DiagnosticLevel::Ok,
            format!("{url} → {} ({flags})", response.status()),
            "",
            None,
        ),
        Ok(response) => check(
            CHECK_LOCAL_SERVER,
            "",
            DiagnosticLevel::Error,
            format!("{url} → {} ({flags})", response.status()),
            HINT_CHECK_FIREWALL,
            None,
        ),
        Err(error) => check(
            CHECK_LOCAL_SERVER,
            "",
            DiagnosticLevel::Error,
            format!("{url} — {error} ({flags})"),
            HINT_CHECK_FIREWALL,
            None,
        ),
    }
}

/// 解析符号链接与父目录引用后精确匹配已知目录；返回同一规范路径交给平台打开，拒绝文件与子目录。
fn resolve_log_dir_target(target: &Path, allowed: &[PathBuf]) -> Option<PathBuf> {
    let target = std::fs::canonicalize(target).ok()?;
    if !target.is_dir() {
        return None;
    }
    allowed
        .iter()
        .any(|root| std::fs::canonicalize(root).is_ok_and(|root| target == root))
        .then_some(target)
}

fn daemon_log_dir(describe: Option<&Value>) -> Option<&str> {
    describe?
        .get("logDir")
        .and_then(Value::as_str)
        .filter(|dir| !dir.is_empty())
}

/// daemon 会话的 HTTP 基址 → `<基址>/exports/{id}`。
fn daemon_export_url(base: &reqwest::Url, export_id: &str) -> reqwest::Url {
    let mut url = base.clone();
    url.set_path(&format!("/exports/{export_id}"));
    url.set_query(None);
    url
}

fn pretty_json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

async fn spawn_blocking_platform<F>(task: F) -> Result<(), DiagnosticsError>
where
    F: FnOnce() -> Result<(), crate::platform::PlatformError> + Send + 'static,
{
    tokio::task::spawn_blocking(task)
        .await
        .map_err(join_error)?
        .map_err(DiagnosticsError::Platform)
}

fn join_error(error: tokio::task::JoinError) -> DiagnosticsError {
    DiagnosticsError::Io(std::io::Error::other(format!(
        "blocking task failed: {error}"
    )))
}

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticsError {
    #[error("invalid diagnostic repair: {0}")]
    InvalidAction(String),
    #[error("daemon diagnostic RPC failed: {0:?}")]
    Daemon(fluxdown_protocol::RpcErrorData),
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    #[error("diagnostic I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Platform(#[from] crate::platform::PlatformError),
    #[error("log export failed: {0}")]
    Export(String),
    #[error("test notification failed: {0}")]
    Notification(String),
    #[error(transparent)]
    Permission(#[from] crate::permission::PermissionError),
    /// 修复步骤已执行，但重新探测仍未通过。
    #[error("repair did not resolve the problem: {0}")]
    RepairIncomplete(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fluxdown_protocol::capture_link::OpenAssociation;
    use fluxdown_protocol::{
        AgentSnapshot, ComponentProbeDto, DiagnosticLevel, PlatformIntegrationDto, StorageProbeDto,
        StorageProbeFailure, StorageProbeRole,
    };
    use serde_json::json;

    use super::{
        ACTION_ENABLE_AUTOSTART, ACTION_ENABLE_SERVICE, ACTION_FIX_COMPONENT, ACTION_FIX_COMSPEC,
        ACTION_FIX_DIR_ACCESS, ACTION_OPEN_LOG_DIR, ACTION_OPEN_SETTINGS, ACTION_REGISTER,
        ACTION_REREGISTER, ACTION_TEST_NOTIFICATION, ACTION_USE_THIS_INSTALL, DiskSleepBlockers,
        HINT_ASSOCIATION_OFF, HINT_AUTOSTART_BLOCKED, HINT_CHECK_DISK, HINT_COMPONENT_BLOCKED,
        HINT_COMPONENT_BLOCKED_FIXABLE, HINT_COMPONENT_BROKEN, HINT_DAEMON_STARTUP_FAILED,
        HINT_DIR_PERMISSION, HINT_DIR_PERMISSION_MACOS, HINT_DIR_TIMEOUT, HINT_DISK_SLEEP_BLOCKERS,
        HINT_ENABLE_LOCAL_SERVER, HINT_ENABLE_PROTOCOL, HINT_LOW_DISK_SPACE, HINT_NMH_CMD_DISABLED,
        HINT_NMH_COMSPEC_INVALID, HINT_NMH_COMSPEC_RESTART, HINT_NMH_FOREIGN_OWNED,
        HINT_NMH_LAUNCH_FAILED, HINT_NMH_OTHER_INSTALL, HINT_NMH_USER_HOSTS_DISABLED,
        HINT_NOTIFICATIONS_BLOCKED, HINT_REINSTALL_APP, HINT_REREGISTER_NMH, LOW_SPACE_BYTES,
        STARTUP_STDERR_LINES, TARGET_TORRENT, autostart_check, category_probe_targets,
        component_check, comspec_check, daemon_export_url, daemon_log_dir, daemon_startup_check,
        disk_sleep_blockers, disk_sleep_check, manifest_check, nmh_checks, nmh_launch_check,
        notification_check, opted_out_associations, probe_local_server, probe_log_dir, relay_check,
        shell_checks, storage_check,
    };
    use crate::nmh::registry::{NmhDiagnosis, NmhTarget, RelayOwner};
    use crate::permission::Os;

    fn target(label: &str, installed: bool, ok: bool) -> NmhTarget {
        NmhTarget {
            label: label.to_owned(),
            location: format!("/manifests/{label}.json"),
            installed,
            ok,
            issue: if ok {
                String::new()
            } else {
                "manifest file missing".to_owned()
            },
        }
    }

    fn snapshot_with(
        connected: bool,
        seeding_statuses: &[i32],
        rss: &[(bool, i32)],
        idle_scan: Option<&str>,
    ) -> Result<AgentSnapshot, serde_json::Error> {
        let mut snapshot = AgentSnapshot {
            daemon_connected: connected,
            ..AgentSnapshot::default()
        };
        for (index, status) in seeding_statuses.iter().enumerate() {
            snapshot.daemon.tasks.push(serde_json::from_value(json!({
                "taskId": format!("t{index}"),
                "url": "magnet:?xt=urn:btih:x",
                "fileName": "f",
                "saveDir": "/tmp",
                "status": 3,
                "downloadedBytes": 1,
                "totalBytes": 1,
                "errorMessage": "",
                "createdAt": "1",
                "proxyUrl": "",
                "queueId": "",
                "checksum": "",
                "seedingStatus": status,
            }))?);
        }
        for (enabled, minutes) in rss {
            snapshot
                .daemon
                .rss_sources
                .push(serde_json::from_value(json!({
                    "url": "https://feed.test/rss",
                    "enabled": enabled,
                    "intervalMinutes": minutes,
                }))?);
        }
        if let Some(value) = idle_scan {
            snapshot
                .daemon
                .config
                .values
                .insert("idle_file_scan".to_owned(), value.to_owned());
        }
        Ok(snapshot)
    }

    #[test]
    fn disk_sleep_blockers_count_only_active_seeding_and_enabled_rss()
    -> Result<(), serde_json::Error> {
        // 1=做种中；8=排队等待做种槽、2=达分享率停止，均不访问磁盘；禁用的 RSS 源不计。
        // 间隔 0 落回引擎默认 30 分钟。
        let snapshot = snapshot_with(
            true,
            &[1, 1, 8, 2, 0],
            &[(true, 0), (true, 15), (false, 1)],
            None,
        )?;
        assert_eq!(
            disk_sleep_blockers(&snapshot),
            Some(DiskSleepBlockers {
                seeding_tasks: 2,
                rss_sources: 2,
                shortest_rss_minutes: Some(15),
                idle_file_scan: false,
            })
        );
        let defaulted = snapshot_with(true, &[], &[(true, 0)], None)?;
        assert_eq!(
            disk_sleep_blockers(&defaulted).and_then(|b| b.shortest_rss_minutes),
            Some(30)
        );
        Ok(())
    }

    #[test]
    fn disk_sleep_idle_scan_accepts_bool_spellings_and_defaults_off()
    -> Result<(), serde_json::Error> {
        for (raw, expected) in [
            (Some("true"), true),
            (Some("1"), true),
            (Some("TRUE"), true),
            (Some("false"), false),
            (Some("0"), false),
            (None, false),
        ] {
            let snapshot = snapshot_with(true, &[], &[], raw)?;
            assert_eq!(
                disk_sleep_blockers(&snapshot).map(|b| b.idle_file_scan),
                Some(expected),
                "idle_file_scan={raw:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn disk_sleep_is_skipped_while_daemon_is_disconnected() -> Result<(), serde_json::Error> {
        // 断线时快照里的任务 / RSS 是过期数据，不能据此下结论。
        let snapshot = snapshot_with(false, &[1], &[(true, 5)], Some("1"))?;
        assert_eq!(disk_sleep_blockers(&snapshot), None);
        Ok(())
    }

    #[test]
    fn disk_sleep_check_is_ok_when_idle_and_info_listing_every_blocker() {
        let quiet = disk_sleep_check(&DiskSleepBlockers {
            seeding_tasks: 0,
            rss_sources: 0,
            shortest_rss_minutes: None,
            idle_file_scan: false,
        });
        assert_eq!(quiet.id, "disk_sleep");
        assert_eq!(quiet.level, DiagnosticLevel::Ok);
        assert!(quiet.hint.is_empty());
        assert!(quiet.repair.is_none());

        let busy = disk_sleep_check(&DiskSleepBlockers {
            seeding_tasks: 3,
            rss_sources: 2,
            shortest_rss_minutes: Some(10),
            idle_file_scan: true,
        });
        assert_eq!(busy.level, DiagnosticLevel::Info);
        assert_eq!(busy.hint, HINT_DISK_SLEEP_BLOCKERS);
        assert!(busy.repair.is_none());
        for needle in [
            "seeding tasks: 3",
            "enabled RSS sources: 2",
            "shortest interval 10 min",
            "idle file scan: on",
        ] {
            assert!(busy.detail.contains(needle), "{needle} in {}", busy.detail);
        }

        let only_seeding = disk_sleep_check(&DiskSleepBlockers {
            seeding_tasks: 1,
            rss_sources: 0,
            shortest_rss_minutes: None,
            idle_file_scan: false,
        });
        assert!(!only_seeding.detail.contains("RSS"));
        assert!(!only_seeding.detail.contains("idle file scan"));
    }

    #[test]
    fn daemon_startup_check_surfaces_crash_loop_with_a_bounded_stderr_tail() {
        let ok = daemon_startup_check(0, false, "");
        assert_eq!(ok.id, "daemon_startup");
        assert_eq!(ok.level, DiagnosticLevel::Ok);
        assert!(ok.hint.is_empty());

        let stderr = (1..=30)
            .map(|line| format!("startup line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let failing = daemon_startup_check(4, true, &stderr);
        assert_eq!(failing.level, DiagnosticLevel::Error);
        assert_eq!(failing.hint, HINT_DAEMON_STARTUP_FAILED);
        assert!(failing.detail.contains("4"), "{}", failing.detail);
        assert!(failing.detail.contains("startup line 30"));
        assert!(!failing.detail.contains("startup line 1\n"));
        assert!(
            failing.detail.lines().count() <= STARTUP_STDERR_LINES + 2,
            "{}",
            failing.detail
        );

        // 崩溃循环但还没有任何 stderr：仍然报错，只是没有摘要。
        let silent = daemon_startup_check(3, true, "  \n");
        assert_eq!(silent.level, DiagnosticLevel::Error);
    }

    #[test]
    fn nmh_checks_map_levels_hints_and_repairs() {
        let diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            chromium_manifest: "/missing/chromium.json".to_owned(),
            firefox_manifest: "/missing/firefox.json".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::Current,
            targets: vec![
                target("Chrome", true, true),
                target("Edge", true, false),
                target("Firefox", false, false),
            ],
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        assert_eq!(checks.len(), 6);
        assert_eq!(checks[0].id, "nmh_binary");
        assert_eq!(checks[0].level, DiagnosticLevel::Ok);
        assert_eq!(checks[1].id, "nmh_manifest");
        assert_eq!(checks[1].level, DiagnosticLevel::Error);
        assert_eq!(checks[1].hint, HINT_REREGISTER_NMH);
        #[cfg(unix)]
        {
            assert!(checks[1].detail.contains("missing: chromium"));
            assert!(!checks[1].detail.contains("missing: chromium, firefox"));
            assert!(checks[1].detail.contains("firefox: (not installed)"));
        }
        #[cfg(windows)]
        assert!(checks[1].detail.contains("missing: chromium, firefox"));
        assert_eq!(checks[2].id, "nmh_relay");
        assert_eq!(checks[2].level, DiagnosticLevel::Ok);
        assert_eq!(checks[3].target, "Chrome");
        assert_eq!(checks[3].level, DiagnosticLevel::Ok);
        assert!(checks[3].repair.is_none());
        assert_eq!(checks[4].target, "Edge");
        assert_eq!(checks[4].level, DiagnosticLevel::Error);
        assert_eq!(checks[4].hint, HINT_REREGISTER_NMH);
        assert_eq!(
            checks[4].repair.as_ref().map(|r| r.action.as_str()),
            Some(ACTION_REREGISTER)
        );
        assert_eq!(checks[5].target, "Firefox");
        assert_eq!(checks[5].level, DiagnosticLevel::Info);
        assert!(checks[5].detail.contains("browser not installed"));
        assert!(checks[5].hint.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn uninstalled_browser_nmh_manifest_is_not_treated_as_error() {
        // macOS / Linux 上未安装 Firefox 时，只要已安装的 Chromium 清单存在，清单检查即为 Ok (#713)
        let temp_dir = std::env::temp_dir().join(format!(
            "fluxdown_diag_nmh_{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let valid_manifest = temp_dir.join("com.fluxdown.nmh.json");
        std::fs::write(&valid_manifest, "{}").expect("write manifest");

        let diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            chromium_manifest: valid_manifest.display().to_string(),
            firefox_manifest: "/uninstalled/firefox/com.fluxdown.nmh.json".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::Current,
            targets: vec![
                target("Chrome", true, true),
                target("Firefox", false, false),
            ],
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        let manifest = checks.iter().find(|c| c.id == "nmh_manifest").unwrap();
        assert_eq!(manifest.level, DiagnosticLevel::Ok);
        assert!(manifest.detail.contains("firefox: (not installed)"));
        assert_eq!(manifest.repair, None);

        drop(std::fs::remove_dir_all(&temp_dir));
    }

    #[cfg(unix)]
    #[test]
    fn uninstalled_chromium_with_missing_firefox_manifest_reports_firefox_error() {
        let diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            chromium_manifest: "/uninstalled/chromium/manifest.json".to_owned(),
            firefox_manifest: "/missing/firefox/manifest.json".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::Current,
            targets: vec![
                target("Chrome", false, false),
                target("Firefox", true, false),
            ],
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        let manifest = checks.iter().find(|c| c.id == "nmh_manifest").unwrap();
        assert_eq!(manifest.level, DiagnosticLevel::Error);
        assert!(manifest.detail.contains("missing: firefox"));
        assert!(manifest.detail.contains("chromium: (not installed)"));
    }

    #[cfg(unix)]
    #[test]
    fn neither_browser_installed_manifest_check_reports_info() {
        let diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            chromium_manifest: "/uninstalled/chromium/manifest.json".to_owned(),
            firefox_manifest: "/uninstalled/firefox/manifest.json".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::Current,
            targets: vec![
                target("Chrome", false, false),
                target("Firefox", false, false),
            ],
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        let manifest = checks.iter().find(|c| c.id == "nmh_manifest").unwrap();
        assert_eq!(manifest.level, DiagnosticLevel::Info);
        assert!(manifest.detail.contains("chromium: (not installed)"));
        assert!(manifest.detail.contains("firefox: (not installed)"));
        assert_eq!(manifest.repair, None);
    }

    #[test]
    fn relay_owned_by_another_install_is_info_but_broken_relay_is_an_error() {
        let mut diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/other/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::OtherInstall,
            ..NmhDiagnosis::default()
        };
        let other = relay_check(&diagnosis);
        assert_eq!(other.level, DiagnosticLevel::Info);
        assert_eq!(other.hint, HINT_NMH_OTHER_INSTALL);
        assert!(other.detail.contains("/other/fluxdown_nmh"));
        assert_eq!(
            other.repair.as_ref().map(|r| r.action.as_str()),
            Some(ACTION_USE_THIS_INSTALL)
        );
        for owner in [RelayOwner::Broken, RelayOwner::Missing] {
            diagnosis.relay_owner = owner;
            let broken = relay_check(&diagnosis);
            assert_eq!(broken.level, DiagnosticLevel::Error, "{owner:?}");
            assert_eq!(broken.hint, HINT_REREGISTER_NMH);
            assert_eq!(
                broken.repair.as_ref().map(|r| r.action.as_str()),
                Some(ACTION_REREGISTER)
            );
        }
    }

    #[test]
    fn missing_relay_binary_is_an_error_with_reinstall_hint() {
        let diagnosis = NmhDiagnosis {
            exe_error: "fluxdown_nmh not found".to_owned(),
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].level, DiagnosticLevel::Error);
        assert_eq!(checks[0].hint, HINT_REINSTALL_APP);
        assert_eq!(checks[0].detail, "fluxdown_nmh not found");
        let ok = manifest_check("", true, "", true);
        assert_eq!(ok.level, DiagnosticLevel::Error);
    }

    #[test]
    fn shell_checks_follow_platform_integration() {
        let mut integration = PlatformIntegrationDto {
            url_protocol_supported: true,
            file_association_supported: true,
            url_protocols: BTreeMap::from([
                ("fluxdown".to_owned(), false),
                ("magnet".to_owned(), true),
            ]),
            ..PlatformIntegrationDto::default()
        };
        let checks = shell_checks(&integration, &[]);
        assert_eq!(checks.len(), 4);
        let fluxdown = &checks[0];
        assert_eq!(fluxdown.id, "url_protocol");
        assert_eq!(fluxdown.target, "fluxdown");
        assert_eq!(fluxdown.level, DiagnosticLevel::Warn);
        assert_eq!(fluxdown.hint, HINT_ENABLE_PROTOCOL);
        assert_eq!(
            fluxdown
                .repair
                .as_ref()
                .map(|r| (r.action.as_str(), r.target.as_str())),
            Some((ACTION_REGISTER, "fluxdown"))
        );
        assert_eq!(checks[1].target, "magnet");
        assert_eq!(checks[1].level, DiagnosticLevel::Ok);
        assert!(checks[1].repair.is_none());
        assert_eq!(checks[2].target, "ed2k");
        assert_eq!(checks[2].level, DiagnosticLevel::Info);
        let torrent = &checks[3];
        assert_eq!(torrent.id, "torrent_association");
        assert_eq!(torrent.level, DiagnosticLevel::Info);
        assert_eq!(
            torrent.repair.as_ref().map(|r| r.target.as_str()),
            Some(TARGET_TORRENT)
        );

        integration.url_protocol_supported = false;
        integration.file_association_supported = false;
        let checks = shell_checks(&integration, &[OpenAssociation::Magnet]);
        assert!(checks.iter().all(|c| c.level == DiagnosticLevel::Info));
        assert!(
            checks
                .iter()
                .all(|c| c.repair.is_none() && c.hint.is_empty())
        );
    }

    #[test]
    fn opted_out_associations_report_user_intent_without_register_repair() {
        let integration = PlatformIntegrationDto {
            url_protocol_supported: true,
            file_association_supported: true,
            // ed2k / .torrent：系统无其他候选，仍指向 FluxDown；magnet 已移交他人。
            torrent_associated: true,
            url_protocols: BTreeMap::from([
                ("fluxdown".to_owned(), true),
                ("magnet".to_owned(), false),
                ("ed2k".to_owned(), true),
            ]),
            ..PlatformIntegrationDto::default()
        };
        let checks = shell_checks(
            &integration,
            &[
                OpenAssociation::Torrent,
                OpenAssociation::Magnet,
                OpenAssociation::Ed2k,
            ],
        );
        assert!(checks.iter().all(|c| c.repair.is_none()));
        assert_eq!(checks[0].level, DiagnosticLevel::Ok);
        let magnet = &checks[1];
        assert_eq!(magnet.level, DiagnosticLevel::Ok);
        assert!(magnet.hint.is_empty());
        for sticky in [&checks[2], &checks[3]] {
            assert_eq!(sticky.level, DiagnosticLevel::Info);
            assert_eq!(sticky.hint, HINT_ASSOCIATION_OFF);
        }
    }

    #[test]
    fn only_true_opt_out_preferences_count() {
        let mut snapshot = AgentSnapshot::default();
        snapshot.preferences.values.extend([
            ("ed2k_assoc_user_disabled".to_owned(), json!(true)),
            ("magnet_assoc_user_disabled".to_owned(), json!(false)),
        ]);
        assert_eq!(
            opted_out_associations(&snapshot),
            vec![OpenAssociation::Ed2k]
        );
    }

    #[test]
    fn open_log_dir_accepts_only_existing_known_directories() {
        let root = std::env::temp_dir().join(format!("fluxdown_doctor_{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        let logs = data.join("logs");
        let daemon_logs = root.join("daemon-logs");
        let outside = root.join("other");
        let child = logs.join("nested");
        for dir in [&logs, &daemon_logs, &outside, &child] {
            std::fs::create_dir_all(dir).expect("create diagnostic directories");
        }
        let executable = logs.join("downloaded-program");
        std::fs::write(&executable, b"program").expect("create file inside logs");
        let allowed = vec![data.clone(), logs.clone(), daemon_logs.clone()];
        for dir in [&data, &logs, &daemon_logs] {
            assert_eq!(
                super::resolve_log_dir_target(dir, &allowed),
                Some(std::fs::canonicalize(dir).expect("canonical directory"))
            );
        }
        assert_eq!(
            super::resolve_log_dir_target(&logs.join("..").join("logs"), &allowed),
            Some(std::fs::canonicalize(&logs).expect("canonical logs"))
        );
        for rejected in [
            outside,
            child,
            executable.clone(),
            root.join("missing"),
            logs.join("..").join("..").join("other"),
        ] {
            assert_eq!(
                super::resolve_log_dir_target(&rejected, &allowed),
                None,
                "{}",
                rejected.display()
            );
        }
        assert_eq!(
            super::resolve_log_dir_target(&executable, std::slice::from_ref(&executable)),
            None
        );
        std::fs::remove_dir_all(root).expect("remove diagnostic directories");
    }

    #[cfg(unix)]
    #[test]
    fn open_log_dir_resolves_aliases_without_allowing_symlink_escapes() {
        let root = std::env::temp_dir().join(format!("fluxdown_doctor_{}", uuid::Uuid::new_v4()));
        let logs = root.join("logs");
        let outside = root.join("other");
        std::fs::create_dir_all(&logs).expect("create logs");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        let alias = root.join("logs-alias");
        let escape = logs.join("escape");
        std::os::unix::fs::symlink(&logs, &alias).expect("alias logs");
        std::os::unix::fs::symlink(&outside, &escape).expect("symlink outside directory");
        assert_eq!(
            super::resolve_log_dir_target(&alias, std::slice::from_ref(&logs)),
            Some(std::fs::canonicalize(&logs).expect("canonical logs"))
        );
        assert_eq!(super::resolve_log_dir_target(&escape, &[logs]), None);
        std::fs::remove_dir_all(root).expect("remove diagnostic directories");
    }

    #[test]
    fn log_dir_probe_reports_writability() {
        let dir = std::env::temp_dir().join(format!(
            "fluxdown_doctor_{}_{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let missing = probe_log_dir(&dir);
        assert_eq!(missing.level, DiagnosticLevel::Error);
        assert_eq!(missing.hint, HINT_CHECK_DISK);
        std::fs::create_dir_all(&dir).expect("create doctor test log directory");
        std::fs::write(dir.join("agent.log"), b"line\n").expect("write doctor test log");
        let writable = probe_log_dir(&dir);
        assert_eq!(writable.level, DiagnosticLevel::Ok);
        assert!(writable.detail.contains("1 log files"));
        assert_eq!(
            writable.repair.as_ref().map(|r| r.action.as_str()),
            Some(ACTION_OPEN_LOG_DIR)
        );
        assert!(!dir.join(".doctor_write_probe").exists());
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), %error, "doctor test cleanup failed");
        }
    }

    #[tokio::test]
    async fn disabled_local_server_offers_enable_repair() {
        let gateway = fluxdown_protocol::GatewayStatusDto::default();
        let check = probe_local_server(&gateway).await;
        assert_eq!(check.level, DiagnosticLevel::Info);
        assert_eq!(check.hint, HINT_ENABLE_LOCAL_SERVER);
        assert_eq!(
            check.repair.as_ref().map(|r| r.action.as_str()),
            Some(ACTION_ENABLE_SERVICE)
        );
    }

    #[test]
    fn daemon_export_url_and_log_dir_are_derived() {
        let access = crate::daemon_client::DaemonHttpAccess::from_rpc_url(
            "ws://127.0.0.1:17801/rpc?x=1",
            String::new(),
        )
        .expect("session access");
        assert_eq!(
            daemon_export_url(&access.base_url, "logs:abc").to_string(),
            "http://127.0.0.1:17801/exports/logs:abc"
        );
        let describe = serde_json::json!({ "logDir": "/data/logs" });
        assert_eq!(daemon_log_dir(Some(&describe)), Some("/data/logs"));
        assert_eq!(daemon_log_dir(Some(&serde_json::json!({}))), None);
        assert_eq!(daemon_log_dir(None), None);
    }

    fn storage(
        role: StorageProbeRole,
        failure: Option<StorageProbeFailure>,
        available_bytes: Option<u64>,
    ) -> StorageProbeDto {
        StorageProbeDto {
            role,
            label: "Main".to_owned(),
            path: "/data/dl".to_owned(),
            exists: true,
            probed_dir: "/data/dl".to_owned(),
            failed_step: if failure.is_some() { "create" } else { "" }.to_owned(),
            failure,
            error: if failure.is_some() {
                "Permission denied (os error 13)"
            } else {
                ""
            }
            .to_owned(),
            available_bytes,
            os_error: failure.map(|_| 13),
        }
    }

    fn repair_of(check: &fluxdown_protocol::DiagnosticCheckDto) -> Option<(String, String)> {
        check
            .repair
            .as_ref()
            .map(|repair| (repair.action.clone(), repair.target.clone()))
    }

    #[test]
    fn storage_probe_failures_and_low_space_need_attention() {
        let ok = storage_check(
            &storage(StorageProbeRole::Queue, None, Some(LOW_SPACE_BYTES)),
            true,
            Os::Linux,
        );
        assert_eq!(
            (ok.id.as_str(), ok.target.as_str()),
            ("queue_save_dir", "Main")
        );
        assert_eq!((ok.level, repair_of(&ok)), (DiagnosticLevel::Ok, None));

        let low = storage_check(
            &storage(
                StorageProbeRole::DefaultSaveDir,
                None,
                Some(LOW_SPACE_BYTES - 1),
            ),
            true,
            Os::Linux,
        );
        assert_eq!(low.id, "save_dir");
        assert_eq!(low.level, DiagnosticLevel::Warn);
        assert_eq!(low.hint, HINT_LOW_DISK_SPACE);

        let full = storage_check(
            &storage(
                StorageProbeRole::Rss,
                Some(StorageProbeFailure::NoSpace),
                None,
            ),
            true,
            Os::Linux,
        );
        assert_eq!(
            (full.hint.as_str(), repair_of(&full)),
            (HINT_LOW_DISK_SPACE, None)
        );
        let offline = storage_check(
            &storage(
                StorageProbeRole::Category,
                Some(StorageProbeFailure::Timeout),
                None,
            ),
            true,
            Os::Linux,
        );
        assert_eq!(offline.id, "category_save_dir");
        assert_eq!(offline.hint, HINT_DIR_TIMEOUT);
    }

    #[test]
    fn permission_denied_dirs_offer_the_repair_matching_the_cause() {
        let denied = storage(
            StorageProbeRole::DataDir,
            Some(StorageProbeFailure::PermissionDenied),
            Some(u64::MAX),
        );
        let unix = storage_check(&denied, true, Os::Linux);
        assert_eq!(unix.id, "data_dir");
        assert_eq!(unix.level, DiagnosticLevel::Error);
        assert!(unix.detail.contains("create failed: Permission denied"));
        assert_eq!(unix.hint, HINT_DIR_PERMISSION);
        assert_eq!(
            repair_of(&unix),
            Some((ACTION_FIX_DIR_ACCESS.to_owned(), "/data/dl".to_owned()))
        );
        // headless：没有授权对话框，不给按钮。
        assert_eq!(repair_of(&storage_check(&denied, false, Os::Linux)), None);

        let mut tcc = denied.clone();
        tcc.os_error = Some(1);
        let mac = storage_check(&tcc, true, Os::MacOs);
        assert_eq!(mac.hint, HINT_DIR_PERMISSION_MACOS);
        assert_eq!(
            repair_of(&mac),
            Some((
                ACTION_OPEN_SETTINGS.to_owned(),
                "files_and_folders".to_owned()
            ))
        );
        // Linux 上的 EPERM（不可变属性等）改属主无效：没有按钮。
        assert_eq!(repair_of(&storage_check(&tcc, true, Os::Linux)), None);

        let mut windows = denied;
        windows.os_error = Some(5);
        assert_eq!(
            repair_of(&storage_check(&windows, true, Os::Windows)).map(|(action, _)| action),
            Some(ACTION_FIX_DIR_ACCESS.to_owned())
        );
    }

    #[test]
    fn missing_save_dir_reports_the_probed_parent() {
        let mut probe = storage(StorageProbeRole::DefaultSaveDir, None, Some(u64::MAX));
        probe.exists = false;
        probe.probed_dir = "/data".to_owned();
        let check = storage_check(&probe, true, Os::Linux);
        assert_eq!(check.level, DiagnosticLevel::Ok);
        assert!(check.detail.contains("probed /data"));
    }

    #[test]
    fn component_launch_denial_and_runtime_failure_get_distinct_hints() {
        let mut probe = ComponentProbeDto {
            name: "yt-dlp".to_owned(),
            path: "/bin/yt-dlp".to_owned(),
            output: "2026.07.04".to_owned(),
            error: String::new(),
            permission_denied: false,
            managed: true,
        };
        let ok = component_check(&probe, Os::Linux);
        assert_eq!(
            (ok.target.as_str(), ok.level),
            ("yt-dlp", DiagnosticLevel::Ok)
        );
        probe.output.clear();
        probe.error = "spawn failed: Permission denied".to_owned();
        probe.permission_denied = true;
        let denied = component_check(&probe, Os::Linux);
        assert_eq!(denied.level, DiagnosticLevel::Error);
        assert_eq!(denied.hint, HINT_COMPONENT_BLOCKED_FIXABLE);
        assert_eq!(
            repair_of(&denied),
            Some((ACTION_FIX_COMPONENT.to_owned(), "yt-dlp".to_owned()))
        );
        // Windows 没有执行位；手动 / 系统路径不替用户改：都不给按钮，提示里也不提按钮。
        let windows = component_check(&probe, Os::Windows);
        assert_eq!(
            (windows.hint.as_str(), repair_of(&windows)),
            (HINT_COMPONENT_BLOCKED, None)
        );
        probe.managed = false;
        let manual = component_check(&probe, Os::Linux);
        assert_eq!(
            (manual.hint.as_str(), repair_of(&manual)),
            (HINT_COMPONENT_BLOCKED, None)
        );
        probe.permission_denied = false;
        assert_eq!(
            component_check(&probe, Os::Linux).hint,
            HINT_COMPONENT_BROKEN
        );
    }

    #[test]
    fn only_system_disabled_autostart_needs_attention() {
        use crate::platform::AutostartState;
        let off = autostart_check(AutostartState::Off);
        assert_eq!((off.level, repair_of(&off)), (DiagnosticLevel::Ok, None));
        assert_eq!(
            autostart_check(AutostartState::Enabled).level,
            DiagnosticLevel::Ok
        );
        let blocked = autostart_check(AutostartState::DisabledBySystem);
        assert_eq!(blocked.level, DiagnosticLevel::Warn);
        assert_eq!(blocked.hint, HINT_AUTOSTART_BLOCKED);
        assert_eq!(
            repair_of(&blocked).map(|(action, _)| action),
            Some(ACTION_ENABLE_AUTOSTART.to_owned())
        );
    }

    #[test]
    fn notification_check_offers_test_only_when_a_notification_can_be_sent() {
        use crate::notification::NotificationAvailability;
        let action = |check: &fluxdown_protocol::DiagnosticCheckDto| {
            check.repair.as_ref().map(|repair| repair.action.clone())
        };
        let off = notification_check(None);
        assert_eq!((off.level, action(&off)), (DiagnosticLevel::Ok, None));
        let blocked = notification_check(Some(NotificationAvailability::Blocked("off".to_owned())));
        assert_eq!(blocked.level, DiagnosticLevel::Warn);
        assert_eq!(blocked.hint, HINT_NOTIFICATIONS_BLOCKED);
        // 有通知设置页的平台引导去放行，否则只能发测试通知。
        let expected = if crate::platform::SettingsPane::Notifications.uri().is_some() {
            ACTION_OPEN_SETTINGS
        } else {
            ACTION_TEST_NOTIFICATION
        };
        assert_eq!(action(&blocked).as_deref(), Some(expected));
        let missing = notification_check(Some(NotificationAvailability::Unavailable(
            "no dbus".to_owned(),
        )));
        assert_eq!(missing.level, DiagnosticLevel::Warn);
        assert_eq!(action(&missing), None);
        let unknown = notification_check(Some(NotificationAvailability::Unverifiable));
        assert_eq!(unknown.level, DiagnosticLevel::Info);
        assert_eq!(action(&unknown).as_deref(), Some(ACTION_TEST_NOTIFICATION));
    }

    #[test]
    fn nmh_ownership_and_policy_blocks_are_reported() {
        use crate::nmh::registry::PolicyBlock;
        let diagnosis = NmhDiagnosis {
            exe_path: "/app/fluxdown_nmh".to_owned(),
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            relay_owner: RelayOwner::Current,
            foreign_owned: vec!["/data/fluxdown_nmh.sh".to_owned()],
            policy_blocks: vec![
                PolicyBlock::UserLevelHostsDisabled {
                    browser: "Chrome".to_owned(),
                },
                PolicyBlock::CommandPromptDisabled,
            ],
            ..NmhDiagnosis::default()
        };
        let checks = nmh_checks(&diagnosis);
        let ownership = checks
            .iter()
            .find(|check| check.id == "nmh_ownership")
            .map(|check| (check.level, check.hint.clone(), repair_of(check)));
        assert_eq!(
            ownership,
            Some((
                DiagnosticLevel::Warn,
                HINT_NMH_FOREIGN_OWNED.to_owned(),
                Some((ACTION_REREGISTER.to_owned(), String::new()))
            ))
        );
        let policies: Vec<_> = checks
            .iter()
            .filter(|check| check.id == "nmh_policy")
            .map(|check| {
                (
                    check.target.as_str(),
                    check.hint.as_str(),
                    check.repair.is_none(),
                )
            })
            .collect();
        assert_eq!(
            policies,
            [
                ("Chrome", HINT_NMH_USER_HOSTS_DISABLED, true),
                ("", HINT_NMH_CMD_DISABLED, true)
            ]
        );
    }

    #[test]
    fn nmh_launch_failure_offers_reregister() {
        use crate::nmh::registry::RelayLaunchError;
        let diagnosis = NmhDiagnosis {
            relay_location: "/data/fluxdown_nmh.sh".to_owned(),
            registered_relay: "/app/fluxdown_nmh".to_owned(),
            ..NmhDiagnosis::default()
        };
        let ok = nmh_launch_check(&diagnosis, &Ok(()));
        assert_eq!((ok.level, repair_of(&ok)), (DiagnosticLevel::Ok, None));
        let failed = nmh_launch_check(
            &diagnosis,
            &Err(RelayLaunchError::Exited("exit status: 126".to_owned())),
        );
        assert_eq!(failed.level, DiagnosticLevel::Error);
        assert_eq!(failed.hint, HINT_NMH_LAUNCH_FAILED);
        assert!(failed.detail.contains("126"));
        assert_eq!(
            repair_of(&failed),
            Some((ACTION_REREGISTER.to_owned(), String::new()))
        );
    }

    #[test]
    fn comspec_check_trusts_the_registry_and_flags_stale_processes() {
        use crate::nmh::comspec::{ComSpecIssue, ComSpecSource, ComSpecState, ConfiguredComSpec};
        let cmd = r"C:\Windows\system32\cmd.exe";
        let default = ConfiguredComSpec {
            source: ComSpecSource::System,
            raw: r"%SystemRoot%\system32\cmd.exe".to_owned(),
            path: cmd.to_owned(),
        };
        let summary = |state: &ComSpecState| {
            let check = comspec_check(state);
            (check.level, check.hint.clone(), repair_of(&check))
        };
        let fix = Some((ACTION_FIX_COMSPEC.to_owned(), String::new()));

        // 用户反馈的现场：用户变量以分隔符结尾，本进程也继承了它。
        let broken = ComSpecState {
            configured: Some(ConfiguredComSpec {
                source: ComSpecSource::User,
                raw: r"C:\Windows\system32\".to_owned(),
                path: r"C:\Windows\system32\".to_owned(),
            }),
            configured_issue: Some(ComSpecIssue::TrailingSeparator),
            process: Some(r"C:\Windows\system32\".to_owned()),
            process_issue: Some(ComSpecIssue::TrailingSeparator),
        };
        assert_eq!(
            summary(&broken),
            (
                DiagnosticLevel::Error,
                HINT_NMH_COMSPEC_INVALID.to_owned(),
                fix.clone()
            )
        );
        assert!(comspec_check(&broken).detail.contains(r"HKCU\Environment"));

        let unset = ComSpecState {
            configured_issue: Some(ComSpecIssue::Missing),
            process_issue: Some(ComSpecIssue::Missing),
            ..ComSpecState::default()
        };
        assert_eq!(summary(&unset).0, DiagnosticLevel::Error);

        // 注册表已修好、本进程仍是旧值：不再报错，提示重启浏览器，并允许再次广播。
        let stale = ComSpecState {
            configured: Some(default.clone()),
            configured_issue: None,
            ..broken.clone()
        };
        assert_eq!(
            summary(&stale),
            (
                DiagnosticLevel::Warn,
                HINT_NMH_COMSPEC_RESTART.to_owned(),
                fix
            )
        );

        // 反过来：本进程正常但注册表被改坏，新启动的浏览器会失败。
        let regressed = ComSpecState {
            process: Some(cmd.to_owned()),
            process_issue: None,
            ..broken
        };
        assert_eq!(summary(&regressed).0, DiagnosticLevel::Error);

        let healthy = ComSpecState {
            configured: Some(default),
            configured_issue: None,
            process: Some(cmd.to_owned()),
            process_issue: None,
        };
        assert_eq!(
            summary(&healthy),
            (DiagnosticLevel::Ok, String::new(), None)
        );
    }

    #[test]
    fn category_targets_skip_hidden_and_unset_dirs() -> Result<(), serde_json::Error> {
        let mut snapshot = AgentSnapshot::default();
        snapshot.preferences.values.insert(
            fluxdown_protocol::CUSTOM_CATEGORIES_PREF_KEY.to_owned(),
            json!([
                {"id": "builtin_video", "name": "", "builtinType": "video", "saveDir": "/dl/Video"},
                {"id": "c1", "name": "ISO", "saveDir": "/dl/ISO", "visible": false},
                {"id": "c2", "name": "Docs", "saveDir": " "},
                {"id": "c3", "name": "Books", "saveDir": "/dl/Books"},
            ]),
        );
        let targets = category_probe_targets(&snapshot);
        let labels: Vec<_> = targets
            .iter()
            .map(|target| (target.label.as_str(), target.path.as_str()))
            .collect();
        assert_eq!(labels, vec![("video", "/dl/Video"), ("Books", "/dl/Books")]);
        assert!(
            targets
                .iter()
                .all(|target| target.role == StorageProbeRole::Category)
        );
        Ok(())
    }
}
