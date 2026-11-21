//! FluxDown CLI 入口 —— aria2c 风格命令行下载客户端。

use std::path::{Path, PathBuf};
use std::process::ExitCode as ProcExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use fluxdown_cli::client::{ApiClient, ClientError};
use fluxdown_cli::config::{CliConfig, ConfigError};
use fluxdown_cli::exit::ExitCode;
use fluxdown_cli::format::{
    human_bytes, human_time, percent, rss_item_status_name, status_name, truncate,
};
use fluxdown_engine::logger::{self, startup};
use fluxdown_protocol::daemon::{CreateTaskRequest, RssSourceDto};

/// mimalloc 全局分配器：`--local` 内嵌引擎时吞吐与内存碎片均优于 musl/glibc 默认分配器。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod local;

/// 默认服务基址（本机 API 服务，仅监听 127.0.0.1）。
const DEFAULT_URL: &str = "http://127.0.0.1:17800";

/// 默认请求超时（秒），当命令行/环境/持久化配置都未指定时使用。
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// FluxDown 命令行下载客户端。
#[derive(Debug, Parser)]
#[command(name = "fluxdown", version = fluxdown_protocol::APP_VERSION, about, long_about = None)]
struct Cli {
    /// 服务基址（默认 http://127.0.0.1:17800）。
    #[arg(long, global = true, env = "FLUXDOWN_URL")]
    url: Option<String>,

    /// 管理 API token。
    #[arg(long, global = true, env = "FLUXDOWN_TOKEN")]
    token: Option<String>,

    /// 请求超时（秒，默认 30；可用 `config set timeout` 持久化）。
    #[arg(long, global = true)]
    timeout: Option<u64>,

    /// 以 JSON 输出（脚本友好）。
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 探测服务是否存活（无需 token）。
    Ping,
    /// 显示服务应用信息。
    Info,
    /// 新增下载任务。
    #[command(visible_alias = "get")]
    Add(Box<AddArgs>),
    /// 列出任务。
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// 查看单个任务详情。
    #[command(visible_alias = "stat")]
    Status { id: String },
    /// 暂停任务。
    Pause { id: String },
    /// 恢复任务。
    Resume { id: String },
    /// 删除任务。
    Rm(RmArgs),
    /// 暂停全部任务。
    PauseAll,
    /// 恢复全部任务。
    ResumeAll,
    /// 列出命名队列。
    Queue,
    /// 管理 RSS 订阅（自动追番/自动下载）。
    Rss(RssArgs),
    /// 轮询显示任务进度直至完成/出错。
    Watch(WatchArgs),
    /// 读写持久化 CLI 配置（类似 `go env -w`：一次设置长期生效）。
    Config(ConfigArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// 下载 URL（可多个）。
    urls: Vec<String>,
    /// 从文件读取 URL（每行一个，`#` 起始为注释；`-` 表示 stdin）。
    #[arg(short = 'i', long = "input-file")]
    input_file: Option<String>,
    /// 保存目录（空 = 服务端默认）。
    #[arg(short = 'd', long = "dir")]
    dir: Option<String>,
    /// 输出文件名（仅单 URL 时生效）。
    #[arg(short = 'o', long = "out")]
    out: Option<String>,
    /// 分段/线程数（0 = 自动）。
    #[arg(short = 's', long = "segments")]
    segments: Option<i32>,
    /// 单任务代理 URL。
    #[arg(long)]
    proxy: Option<String>,
    /// User-Agent。
    #[arg(short = 'U', long = "user-agent")]
    user_agent: Option<String>,
    /// Referrer。
    #[arg(long)]
    referrer: Option<String>,
    /// Cookies。
    #[arg(long)]
    cookies: Option<String>,
    /// 队列 ID（空 = 默认队列）。
    #[arg(long)]
    queue: Option<String>,
    /// Checksum 校验，格式 `algo=hexhash`。
    #[arg(long)]
    checksum: Option<String>,
    /// HTTP Basic 认证用户名（aria2 `--http-user` 同义）。
    #[arg(long = "http-user")]
    http_user: Option<String>,
    /// HTTP Basic 认证密码（aria2 `--http-passwd` 同义）。
    #[arg(long = "http-passwd")]
    http_passwd: Option<String>,
    /// 为此网站保存认证凭据（需同时给出 `--http-user`），后续同站点
    /// 任务未显式提供凭据时自动套用。
    #[arg(long = "save-auth")]
    save_auth: bool,
    /// 稍后下载：创建任务但不开始（aria2 `pause` 语义，进入所属队列
    /// 等待「启动队列」或手动恢复）。与 `--local` 互斥。
    #[arg(long)]
    pause: bool,
    /// 不连接运行中的服务，在本进程内嵌下载引擎独立完成下载
    /// （一次性阻塞至完成/失败；Ctrl-C 中断为暂停并退出，退出码 7）。
    #[arg(long)]
    local: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// 按状态过滤：pending/downloading/paused/completed/error/preparing 或 0-5。
    #[arg(long)]
    status: Option<String>,
}

#[derive(Debug, Args)]
struct RmArgs {
    /// 任务 ID。
    id: String,
    /// 同时删除磁盘文件。
    #[arg(long)]
    delete_files: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    /// 只监视指定任务 ID（省略 = 监视全部活动任务）。
    id: Option<String>,
    /// 刷新间隔（秒）。
    #[arg(long, default_value_t = 1)]
    interval: u64,
}

#[derive(Debug, Args)]
struct RssArgs {
    #[command(subcommand)]
    action: RssCmd,
}

#[derive(Debug, Subcommand)]
enum RssCmd {
    /// 列出全部订阅。
    #[command(visible_alias = "ls")]
    List,
    /// 新增订阅。
    Add(Box<RssAddArgs>),
    /// 删除订阅（连带其条目记录）。
    Rm {
        /// 订阅 ID。
        id: String,
    },
    /// 立即抓取一次订阅（不等待下一个周期）。
    Refresh {
        /// 订阅 ID。
        id: String,
    },
    /// 列出订阅的条目流。
    Items {
        /// 订阅 ID。
        id: String,
    },
}

#[derive(Debug, Args)]
struct RssAddArgs {
    // 字段名避开 `url`：全局 `--url` 已占用该 arg id，同名会被 clap 合并成
    // 同一个参数，导致订阅地址把服务基址覆盖掉。
    /// 订阅地址：RSS/Atom 地址，或插件 provider 约定的地址（作为 `ctx.url` 交给插件）。
    #[arg(value_name = "URL")]
    feed_url: String,
    /// 订阅来源 provider ID（省略 = 内置 RSS；插件 provider ID 见 `GET /api/v1/plugins` 的
    /// `subscriptionProviderIds`）。
    #[arg(long, value_name = "ID")]
    provider: Option<String>,
    /// 插件 provider 的专属配置，必须是合法 JSON（原样作为 `ctx.providerConfig` 交给插件）。
    #[arg(long = "provider-config", value_name = "JSON")]
    provider_config: Option<String>,
    /// 订阅名（省略 = 用 feed 标题回填）。
    #[arg(long)]
    name: Option<String>,
    /// 队列 ID（省略 = 默认队列）。
    #[arg(long)]
    queue: Option<String>,
    /// 保存目录（省略 = 队列目录 → 全局目录）。
    #[arg(short = 'd', long = "dir")]
    dir: Option<String>,
    /// 抓取间隔（分钟，省略 = 服务端默认 30）。
    #[arg(long)]
    interval: Option<i32>,
    /// 收集模式：只收集条目供手动挑选，不自动建任务。
    #[arg(long = "no-auto")]
    no_auto: bool,
    /// 包含关键词（`|` = 或，空格 = 且；省略 = 不过滤）。
    #[arg(long)]
    include: Option<String>,
    /// 排除关键词。
    #[arg(long)]
    exclude: Option<String>,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigCmd,
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    /// 设置一个配置项。log_level 取值 auto/debug/info/warn/error，重启后生效。
    Set {
        /// 配置键：url / token / timeout / log_level。
        key: String,
        /// 配置值。
        value: String,
    },
    /// 清除一个配置项。
    Unset {
        /// 配置键：url / token / timeout / log_level。
        key: String,
    },
    /// 读取一个配置项（省略 key 则等同 list）。
    Get {
        /// 配置键：url / token / timeout / log_level。
        key: Option<String>,
    },
    /// 列出全部配置项及其当前值。
    List,
    /// 打印 CLI 配置文件路径（log_level 位于引擎数据目录的 logging.conf）。
    Path,
}

fn main() -> ProcExitCode {
    let override_dir = std::env::var_os("FLUXDOWN_DATA_DIR").map(PathBuf::from);
    let data_dir = fluxdown_engine::data_dir::resolve_data_dir(override_dir.as_deref())
        .map_err(logger::LoggerInitError::ResolveDataDirectory)
        .and_then(|data_dir| {
            logger::init_with_dir(&data_dir)?;
            Ok(data_dir)
        });
    let data_dir = match data_dir {
        Ok(data_dir) => data_dir,
        Err(error) => {
            eprintln!(
                "fluxdown: failed to initialize logger: {}",
                logger::format_error_chain(&error)
            );
            return ProcExitCode::from(ExitCode::Unknown.code() as u8);
        }
    };
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = error.exit_code();
            if error.use_stderr() {
                fluxdown_engine::logger::report_error("cli", "parse arguments", &error);
            }
            if let Err(print_error) = error.print() {
                fluxdown_engine::logger::report_error("cli", "print argument error", &print_error);
            }
            return ProcExitCode::from(u8::try_from(exit_code).unwrap_or(1));
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            fluxdown_engine::logger::report_error("cli", "start runtime", &e);
            eprintln!("fluxdown: failed to start runtime: {e}");
            return ProcExitCode::from(ExitCode::Unknown.code() as u8);
        }
    };
    let code = rt.block_on(run(cli, &data_dir));
    ProcExitCode::from(code as u8)
}

/// 解析状态过滤字符串为状态码。
fn parse_status_filter(s: &str) -> Result<i32, String> {
    match s.to_ascii_lowercase().as_str() {
        "pending" | "0" => Ok(0),
        "downloading" | "1" => Ok(1),
        "paused" | "2" => Ok(2),
        "completed" | "3" => Ok(3),
        "error" | "4" => Ok(4),
        "preparing" | "5" => Ok(5),
        other => Err(format!("unknown status filter: {other}")),
    }
}

/// 从输入文件/stdin 读取 URL 列表。
fn read_url_file(path: &str) -> Result<Vec<String>, String> {
    let content = if path == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?
    };
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

async fn run(cli: Cli, data_dir: &Path) -> i32 {
    let json = cli.json;

    // config 子命令是纯本地文件操作，不连服务器、不需要 token —— 提前分流。
    if let Command::Config(args) = cli.command {
        return match cmd_config(args.action, json, data_dir) {
            Ok(()) => ExitCode::Success.code(),
            Err(e) => {
                fluxdown_engine::logger::report_error("cli", "config command", &e);
                eprintln!("fluxdown: {e}");
                e.exit().code()
            }
        };
    }

    // add --local：内嵌引擎独立下载，在构造 ApiClient 之前分流（不需要 base/token）。
    if matches!(&cli.command, Command::Add(a) if a.local) {
        let Command::Add(a) = cli.command else {
            unreachable!("guarded by matches! above")
        };
        return match local::run_add_local(*a, json, data_dir).await {
            Ok(()) => ExitCode::Success.code(),
            Err(e) => {
                fluxdown_engine::logger::report_error("cli", "local add", &e);
                eprintln!("fluxdown: {e}");
                e.exit.code()
            }
        };
    }

    // 加载持久化配置作为 flag/env 未指定时的兜底（优先级：flag/env > 配置文件 > 默认）。
    // 读取失败不致命：仅告警并退回空配置，仍可用 flag/env/默认驱动。
    let cfg = CliConfig::load().unwrap_or_else(|e| {
        fluxdown_engine::logger::report_warning("cli", "load config", &e);
        eprintln!("fluxdown: warning: {e}");
        CliConfig::default()
    });

    let base = cli
        .url
        .or(cfg.url)
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let token = cli.token.or(cfg.token).unwrap_or_default();
    let timeout_secs = cli.timeout.or(cfg.timeout).unwrap_or(DEFAULT_TIMEOUT_SECS);
    let timeout = Duration::from_secs(timeout_secs.max(1));

    let client = match ApiClient::new(&base, &token, timeout) {
        Ok(c) => c,
        Err(e) => {
            fluxdown_engine::logger::report_error("cli", "initialize client", &e);
            eprintln!("fluxdown: {e}");
            return e.exit.code();
        }
    };

    let result: Result<(), ClientError> = match cli.command {
        Command::Ping => cmd_ping(&client, json).await,
        Command::Info => cmd_info(&client, json).await,
        Command::Add(a) => cmd_add(&client, *a, json).await,
        Command::List(a) => cmd_list(&client, a, json).await,
        Command::Status { id } => cmd_status(&client, &id, json).await,
        Command::Pause { id } => cmd_simple(client.pause_task(&id).await, &format!("paused {id}")),
        Command::Resume { id } => {
            cmd_simple(client.resume_task(&id).await, &format!("resumed {id}"))
        }
        Command::Rm(a) => cmd_simple(
            client.delete_task(&a.id, a.delete_files).await,
            &format!("removed {}", a.id),
        ),
        Command::PauseAll => cmd_simple(client.pause_all().await, "paused all tasks"),
        Command::ResumeAll => cmd_simple(client.resume_all().await, "resumed all tasks"),
        Command::Queue => cmd_queue(&client, json).await,
        Command::Rss(a) => cmd_rss(&client, a.action, json).await,
        Command::Watch(a) => cmd_watch(&client, a).await,
        // Config 已在上方提前返回，此处不可达。
        Command::Config(_) => unreachable!("config handled before client construction"),
    };

    match result {
        Ok(()) => ExitCode::Success.code(),
        Err(e) => {
            fluxdown_engine::logger::report_error("cli", "command", &e);
            eprintln!("fluxdown: {e}");
            e.exit.code()
        }
    }
}

fn cmd_simple(res: Result<(), ClientError>, ok_msg: &str) -> Result<(), ClientError> {
    res.map(|()| println!("{ok_msg}"))
}

/// 处理 `config` 子命令：读写持久化配置文件（纯本地，不连服务器）。
fn cmd_config(action: ConfigCmd, json: bool, data_dir: &Path) -> Result<(), ConfigError> {
    let io_error = |source| ConfigError::Io {
        path: data_dir.join(startup::FILE_NAME).display().to_string(),
        source,
    };
    let read_level = || {
        startup::read_level(data_dir)
            .map(startup::setting_name)
            .map_err(io_error)
    };
    match action {
        ConfigCmd::Set { key, value } if key == startup::KEY => {
            let level =
                startup::parse_setting(&value).map_err(|error| ConfigError::InvalidValue {
                    key,
                    reason: error.to_string(),
                })?;
            startup::write_level(data_dir, level).map_err(io_error)?;
            println!("set {}", startup::KEY);
            Ok(())
        }
        ConfigCmd::Unset { key } if key == startup::KEY => {
            startup::write_level(data_dir, None).map_err(io_error)?;
            println!("unset {}", startup::KEY);
            Ok(())
        }
        ConfigCmd::Get { key: Some(key) } if key == startup::KEY => {
            println!("{}", read_level()?);
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg = CliConfig::load()?;
            cfg.set(&key, &value)?;
            cfg.save()?;
            println!("set {key}");
            Ok(())
        }
        ConfigCmd::Unset { key } => {
            let mut cfg = CliConfig::load()?;
            cfg.unset(&key)?;
            cfg.save()?;
            println!("unset {key}");
            Ok(())
        }
        ConfigCmd::Get { key } => {
            let cfg = CliConfig::load()?;
            match key {
                Some(k) => {
                    let v = cfg.get(&k)?.unwrap_or_default();
                    println!("{v}");
                    Ok(())
                }
                None => print_config_list(&cfg, json, read_level()?),
            }
        }
        ConfigCmd::List => {
            let cfg = CliConfig::load()?;
            print_config_list(&cfg, json, read_level()?)
        }
        ConfigCmd::Path => {
            println!("{}", fluxdown_cli::config::config_path()?.display());
            Ok(())
        }
    }
}

/// 打印全部配置项。`json` 时输出对象，否则每行 `key = value`（未设置显示 `(unset)`）。
fn print_config_list(cfg: &CliConfig, json: bool, log_level: &str) -> Result<(), ConfigError> {
    let entries = cfg.entries();
    if json {
        let mut map: serde_json::Map<String, serde_json::Value> = entries
            .into_iter()
            .map(|(k, v)| {
                let val = v.map_or(serde_json::Value::Null, serde_json::Value::String);
                (k.to_owned(), val)
            })
            .collect();
        map.insert(startup::KEY.to_owned(), log_level.into());
        println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
    } else {
        for (k, v) in entries {
            match v {
                Some(val) => println!("{k} = {val}"),
                None => println!("{k} = (unset)"),
            }
        }
        println!("{} = {log_level}", startup::KEY);
    }
    Ok(())
}

async fn cmd_ping(client: &ApiClient, json: bool) -> Result<(), ClientError> {
    let v = client.ping().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        let app = v.get("app").and_then(|x| x.as_str()).unwrap_or("FluxDown");
        let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or("?");
        println!("pong — {app} {ver}");
    }
    Ok(())
}

async fn cmd_info(client: &ApiClient, json: bool) -> Result<(), ClientError> {
    let info = client.info().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&info).unwrap_or_default()
        );
    } else {
        println!("{} {}", info.name, info.version);
    }
    Ok(())
}

async fn cmd_add(client: &ApiClient, a: AddArgs, json: bool) -> Result<(), ClientError> {
    let mut urls = a.urls.clone();
    if let Some(f) = &a.input_file {
        match read_url_file(f) {
            Ok(mut extra) => urls.append(&mut extra),
            Err(e) => {
                return Err(ClientError {
                    message: e,
                    exit: ExitCode::BadRequest,
                });
            }
        }
    }
    if urls.is_empty() {
        return Err(ClientError {
            message: "no URLs given (pass URLs or -i <file>)".to_string(),
            exit: ExitCode::BadRequest,
        });
    }
    let multi = urls.len() > 1;
    let mut created: Vec<String> = Vec::with_capacity(urls.len());
    // best-effort：逐个尝试所有 URL，单条失败不丢弃已建任务，也不中断其余 URL
    // （对齐 aria2 `-i` 行为）。记录首个错误，末尾先汇报已建 id 再据此返回退出码。
    let mut first_err: Option<ClientError> = None;
    for url in urls {
        let req = CreateTaskRequest {
            url: url.clone(),
            file_name: if multi {
                String::new()
            } else {
                a.out.clone().unwrap_or_default()
            },
            save_dir: a.dir.clone().unwrap_or_default(),
            segments: a.segments.unwrap_or(0),
            cookies: a.cookies.clone().unwrap_or_default(),
            referrer: a.referrer.clone().unwrap_or_default(),
            proxy_url: a.proxy.clone().unwrap_or_default(),
            user_agent: a.user_agent.clone().unwrap_or_default(),
            queue_id: a.queue.clone().unwrap_or_default(),
            checksum: a.checksum.clone().unwrap_or_default(),
            ignore_tls_errors: false,
            headers: None,
            torrent_b64: None,
            method: None,
            body: None,
            audio_url: None,
            start_paused: a.pause,
            http_user: a.http_user.clone().unwrap_or_default(),
            http_password: a.http_passwd.clone().unwrap_or_default(),
            save_site_auth: a.save_auth,
        };
        match client.create_task(&req).await {
            Ok(res) => created.push(res.task_id),
            Err(e) => {
                eprintln!("fluxdown: failed to add {url}: {e}");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&created).unwrap_or_default()
        );
    } else {
        for id in &created {
            println!("added {id}");
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

async fn cmd_list(client: &ApiClient, a: ListArgs, json: bool) -> Result<(), ClientError> {
    let status = match a.status.as_deref() {
        Some(s) => Some(parse_status_filter(s).map_err(|m| ClientError {
            message: m,
            exit: ExitCode::BadRequest,
        })?),
        None => None,
    };
    let tasks = client.list_tasks(status).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&tasks).unwrap_or_default()
        );
        return Ok(());
    }
    if tasks.is_empty() {
        println!("(no tasks)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<12}  {:>7}  {:>11}  NAME",
        "ID", "STATUS", "PROG", "SIZE"
    );
    for t in &tasks {
        println!(
            "{:<36}  {:<12}  {:>7}  {:>11}  {}",
            t.task_id,
            status_name(t.status),
            percent(t.downloaded_bytes, t.total_bytes),
            human_bytes(t.total_bytes),
            truncate(&t.file_name, 48),
        );
    }
    Ok(())
}

async fn cmd_status(client: &ApiClient, id: &str, json: bool) -> Result<(), ClientError> {
    let t = client.get_task(id).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&t).unwrap_or_default());
        return Ok(());
    }
    println!("ID:        {}", t.task_id);
    println!("Name:      {}", t.file_name);
    println!("URL:       {}", t.url);
    println!("Dir:       {}", t.save_dir);
    println!("Status:    {}", status_name(t.status));
    println!(
        "Progress:  {} ({} / {})",
        percent(t.downloaded_bytes, t.total_bytes),
        human_bytes(t.downloaded_bytes),
        human_bytes(t.total_bytes)
    );
    if !t.queue_id.is_empty() {
        println!("Queue:     {}", t.queue_id);
    }
    if !t.proxy_url.is_empty() {
        println!("Proxy:     {}", t.proxy_url);
    }
    if !t.checksum.is_empty() {
        println!("Checksum:  {}", t.checksum);
    }
    if !t.error_message.is_empty() {
        println!("Error:     {}", t.error_message);
    }
    Ok(())
}

async fn cmd_queue(client: &ApiClient, json: bool) -> Result<(), ClientError> {
    let queues = client.list_queues().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&queues).unwrap_or_default()
        );
        return Ok(());
    }
    if queues.is_empty() {
        println!("(no queues)");
        return Ok(());
    }
    println!(
        "{:<16}  {:<20}  {:>8}  {:>10}  {:>10}  {:>8}  {:<11}",
        "ID", "NAME", "STATE", "DL/s", "UL/s", "CONCUR", "SCHEDULE"
    );
    for q in &queues {
        let limit = if q.speed_limit_kbps > 0 {
            human_bytes(q.speed_limit_kbps * 1024)
        } else {
            "∞".to_string()
        };
        let up_limit = if q.upload_limit_kbps > 0 {
            human_bytes(q.upload_limit_kbps * 1024)
        } else {
            "∞".to_string()
        };
        let concur = if q.max_concurrent > 0 {
            q.max_concurrent.to_string()
        } else {
            "auto".to_string()
        };
        let state = if q.is_running { "running" } else { "stopped" };
        let schedule = if q.schedule_enabled {
            format!(
                "{}-{}",
                if q.schedule_start.is_empty() {
                    "--:--"
                } else {
                    &q.schedule_start
                },
                if q.schedule_stop.is_empty() {
                    "--:--"
                } else {
                    &q.schedule_stop
                },
            )
        } else {
            "-".to_string()
        };
        println!(
            "{:<16}  {:<20}  {:>8}  {:>10}  {:>10}  {:>8}  {:<11}",
            q.queue_id,
            truncate(&q.name, 20),
            state,
            limit,
            up_limit,
            concur,
            schedule
        );
    }
    Ok(())
}

/// 处理 `rss` 子命令组：全部经 REST 打服务端（无 `--local` 变体——订阅是
/// 常驻轮询语义，脱离运行中的服务没有意义）。
async fn cmd_rss(client: &ApiClient, action: RssCmd, json: bool) -> Result<(), ClientError> {
    match action {
        RssCmd::List => cmd_rss_list(client, json).await,
        RssCmd::Add(a) => cmd_rss_add(client, *a, json).await,
        RssCmd::Rm { id } => cmd_simple(
            client.delete_rss_source(&id).await,
            &format!("removed {id}"),
        ),
        RssCmd::Refresh { id } => cmd_simple(
            client.refresh_rss_source(&id).await,
            &format!("refreshing {id}"),
        ),
        RssCmd::Items { id } => cmd_rss_items(client, &id, json).await,
    }
}

async fn cmd_rss_list(client: &ApiClient, json: bool) -> Result<(), ClientError> {
    let sources = client.list_rss_sources().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&sources).unwrap_or_default()
        );
        return Ok(());
    }
    if sources.is_empty() {
        println!("(no rss sources)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<32}  {:>6}  {:>6}  STATE",
        "ID", "NAME", "EVERY", "UNREAD"
    );
    for s in &sources {
        // interval 0 是「未指定」，服务端会归一为引擎默认值——这里同样展开，
        // 免得表格里出现一列 `0m` 让人以为是「不抓取」。
        let minutes = if s.interval_minutes > 0 {
            s.interval_minutes
        } else {
            fluxdown_engine::rss::model::DEFAULT_INTERVAL_MINUTES
        };
        println!(
            "{:<36}  {:<32}  {:>6}  {:>6}  {}",
            s.source_id,
            truncate(if s.name.is_empty() { &s.url } else { &s.name }, 32),
            format!("{minutes}m"),
            s.unread_count,
            rss_source_state(s),
        );
    }
    Ok(())
}

/// 订阅状态列。STATE 放最后一列，因为报错态带 ANSI 转义，参与 `{:<n}`
/// 对齐会把不可见字节算进宽度、把整张表撑歪。
fn rss_source_state(s: &RssSourceDto) -> String {
    if !s.enabled {
        return "disabled".to_string();
    }
    if !s.last_error.is_empty() {
        // 抓取失败标红：一屏几十条订阅时，红色是唯一能一眼扫到的信号。
        return format!(
            "\x1b[31merror ({}x): {}\x1b[0m",
            s.fail_count,
            truncate(&s.last_error, 48)
        );
    }
    if s.auto_download {
        "auto".to_string()
    } else {
        "collect".to_string()
    }
}

async fn cmd_rss_add(client: &ApiClient, a: RssAddArgs, json: bool) -> Result<(), ClientError> {
    let provider_id = a
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(fluxdown_engine::rss::model::RSS_PROVIDER_ID)
        .to_string();
    let provider_config = a.provider_config.as_deref().map_or("", str::trim);
    if provider_id == fluxdown_engine::rss::model::RSS_PROVIDER_ID && !provider_config.is_empty() {
        return Err(ClientError {
            message: "--provider-config requires --provider <plugin provider id>".to_string(),
            exit: ExitCode::BadRequest,
        });
    }
    if !provider_config.is_empty()
        && let Err(e) = serde_json::from_str::<serde_json::Value>(provider_config)
    {
        return Err(ClientError {
            message: format!("--provider-config is not valid JSON: {e}"),
            exit: ExitCode::BadRequest,
        });
    }
    let req = RssSourceDto {
        source_id: String::new(),
        provider_id,
        provider_config: provider_config.to_string(),
        url: a.feed_url,
        name: a.name.unwrap_or_default(),
        enabled: true,
        auto_download: !a.no_auto,
        start_paused: false,
        queue_id: a.queue.unwrap_or_default(),
        save_dir: a.dir.unwrap_or_default(),
        // 负数按「未指定」处理，交服务端归一为默认间隔。
        interval_minutes: a.interval.unwrap_or(0).max(0),
        include_pattern: a.include.unwrap_or_default(),
        exclude_pattern: a.exclude.unwrap_or_default(),
        use_regex: false,
        smart_episode: false,
        size_min_bytes: 0,
        size_max_bytes: 0,
        send_referer: true,
        notify_on_download: true,
        max_per_fetch: 0,
        cookies: String::new(),
        user_agent: String::new(),
        proxy_url: String::new(),
        // 以下为只读运行态字段，服务端忽略写入值。
        last_fetch_at: 0,
        last_success_at: 0,
        last_error: String::new(),
        fail_count: 0,
        seeded: false,
        position: 0,
        unread_count: 0,
    };
    let id = client.create_rss_source(&req).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "sourceId": id }))
                .unwrap_or_default()
        );
    } else {
        println!("added {id}");
    }
    Ok(())
}

async fn cmd_rss_items(client: &ApiClient, id: &str, json: bool) -> Result<(), ClientError> {
    let items = client.list_rss_items(id).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&items).unwrap_or_default()
        );
        return Ok(());
    }
    if items.is_empty() {
        println!("(no items)");
        return Ok(());
    }
    println!(
        "{:<10}  {:<52}  {:>11}  PUBLISHED",
        "STATUS", "TITLE", "SIZE"
    );
    for i in &items {
        println!(
            "{:<10}  {:<52}  {:>11}  {}",
            rss_item_status_name(i.status),
            truncate(&i.title, 52),
            human_bytes(i.enclosure_length),
            human_time(i.pub_date),
        );
    }
    Ok(())
}

async fn cmd_watch(client: &ApiClient, a: WatchArgs) -> Result<(), ClientError> {
    let interval = Duration::from_secs(a.interval.max(1));
    loop {
        let tasks = match &a.id {
            Some(id) => vec![client.get_task(id).await?],
            None => {
                let mut all = client.list_tasks(None).await?;
                // 只保留活动/未终态任务
                all.retain(|t| matches!(t.status, 0 | 1 | 2 | 5));
                all
            }
        };
        // 清屏 + 光标归位（ANSI）。
        print!("\x1b[2J\x1b[H");
        if tasks.is_empty() {
            println!("(no active tasks)");
            return Ok(());
        }
        println!(
            "{:<36}  {:<12}  {:>7}  {:>11}  NAME",
            "ID", "STATUS", "PROG", "SIZE"
        );
        let mut all_done = true;
        for t in &tasks {
            if matches!(t.status, 0 | 1 | 2 | 5) {
                all_done = false;
            }
            println!(
                "{:<36}  {:<12}  {:>7}  {:>11}  {}",
                t.task_id,
                status_name(t.status),
                percent(t.downloaded_bytes, t.total_bytes),
                human_bytes(t.total_bytes),
                truncate(&t.file_name, 48),
            );
        }
        if all_done {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}
