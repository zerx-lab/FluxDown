//! FluxDown CLI 入口 —— aria2c 风格命令行下载客户端。

use std::process::ExitCode as ProcExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use fluxdown_api::types::CreateTaskRequest;
use fluxdown_cli::client::{ApiClient, ClientError};
use fluxdown_cli::config::{CliConfig, ConfigError};
use fluxdown_cli::exit::ExitCode;
use fluxdown_cli::format::{human_bytes, percent, status_name, truncate};

mod local;

/// 默认服务基址（本机 API 服务，仅监听 127.0.0.1）。
const DEFAULT_URL: &str = "http://127.0.0.1:17800";

/// 默认请求超时（秒），当命令行/环境/持久化配置都未指定时使用。
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// FluxDown 命令行下载客户端。
#[derive(Debug, Parser)]
#[command(name = "fluxdown", version, about, long_about = None)]
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
struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigCmd,
}

#[derive(Debug, Subcommand)]
enum ConfigCmd {
    /// 设置一个配置项（url / token / timeout）。
    Set {
        /// 配置键：url / token / timeout。
        key: String,
        /// 配置值。
        value: String,
    },
    /// 清除一个配置项。
    Unset {
        /// 配置键：url / token / timeout。
        key: String,
    },
    /// 读取一个配置项（省略 key 则等同 list）。
    Get {
        /// 配置键：url / token / timeout。
        key: Option<String>,
    },
    /// 列出全部配置项及其当前值。
    List,
    /// 打印配置文件路径。
    Path,
}

fn main() -> ProcExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("fluxdown: failed to start runtime: {e}");
            return ProcExitCode::from(ExitCode::Unknown.code() as u8);
        }
    };
    let code = rt.block_on(run(cli));
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

async fn run(cli: Cli) -> i32 {
    let json = cli.json;

    // config 子命令是纯本地文件操作，不连服务器、不需要 token —— 提前分流。
    if let Command::Config(args) = cli.command {
        return match cmd_config(args.action, json) {
            Ok(()) => ExitCode::Success.code(),
            Err(e) => {
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
        return match local::run_add_local(*a, json).await {
            Ok(()) => ExitCode::Success.code(),
            Err(e) => {
                eprintln!("fluxdown: {e}");
                e.exit.code()
            }
        };
    }

    // 加载持久化配置作为 flag/env 未指定时的兜底（优先级：flag/env > 配置文件 > 默认）。
    // 读取失败不致命：仅告警并退回空配置，仍可用 flag/env/默认驱动。
    let cfg = CliConfig::load().unwrap_or_else(|e| {
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
        Command::Watch(a) => cmd_watch(&client, a).await,
        // Config 已在上方提前返回，此处不可达。
        Command::Config(_) => unreachable!("config handled before client construction"),
    };

    match result {
        Ok(()) => ExitCode::Success.code(),
        Err(e) => {
            eprintln!("fluxdown: {e}");
            e.exit.code()
        }
    }
}

fn cmd_simple(res: Result<(), ClientError>, ok_msg: &str) -> Result<(), ClientError> {
    res.map(|()| println!("{ok_msg}"))
}

/// 处理 `config` 子命令：读写持久化配置文件（纯本地，不连服务器）。
fn cmd_config(action: ConfigCmd, json: bool) -> Result<(), ConfigError> {
    match action {
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
                None => print_config_list(&cfg, json),
            }
        }
        ConfigCmd::List => {
            let cfg = CliConfig::load()?;
            print_config_list(&cfg, json)
        }
        ConfigCmd::Path => {
            println!("{}", fluxdown_cli::config::config_path()?.display());
            Ok(())
        }
    }
}

/// 打印全部配置项。`json` 时输出对象，否则每行 `key = value`（未设置显示 `(unset)`）。
fn print_config_list(cfg: &CliConfig, json: bool) -> Result<(), ConfigError> {
    let entries = cfg.entries();
    if json {
        let map: serde_json::Map<String, serde_json::Value> = entries
            .iter()
            .map(|(k, v)| {
                let val = v
                    .clone()
                    .map_or(serde_json::Value::Null, serde_json::Value::String);
                ((*k).to_string(), val)
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&map).unwrap_or_default());
    } else {
        for (k, v) in entries {
            match v {
                Some(val) => println!("{k} = {val}"),
                None => println!("{k} = (unset)"),
            }
        }
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
        "{:<10}  {:<12}  {:>7}  {:>11}  NAME",
        "ID", "STATUS", "PROG", "SIZE"
    );
    for t in &tasks {
        println!(
            "{:<10}  {:<12}  {:>7}  {:>11}  {}",
            truncate(&t.task_id, 10),
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
        "{:<16}  {:<20}  {:>8}  {:>10}  {:>8}  {:<11}",
        "ID", "NAME", "STATE", "LIMIT/s", "CONCUR", "SCHEDULE"
    );
    for q in &queues {
        let limit = if q.speed_limit_kbps > 0 {
            human_bytes(q.speed_limit_kbps * 1024)
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
            "{:<16}  {:<20}  {:>8}  {:>10}  {:>8}  {:<11}",
            truncate(&q.queue_id, 16),
            truncate(&q.name, 20),
            state,
            limit,
            concur,
            schedule
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
            "{:<10}  {:<12}  {:>7}  {:>11}  NAME",
            "ID", "STATUS", "PROG", "SIZE"
        );
        let mut all_done = true;
        for t in &tasks {
            if matches!(t.status, 0 | 1 | 2 | 5) {
                all_done = false;
            }
            println!(
                "{:<10}  {:<12}  {:>7}  {:>11}  {}",
                truncate(&t.task_id, 10),
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
