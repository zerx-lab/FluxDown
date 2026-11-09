//! aria2c 命令行解析（#299）：粘贴 `aria2c ...` 命令文本即可建任务。
//!
//! 纯规则，不依赖 GPUI：
//! - 行首（第一个词，可带路径、忽略大小写）为 `aria2c` / `aria2c.exe` 才视为命令，其余行完全不受影响；
//! - 按 shell 规则切词（单引号、双引号、反斜杠转义），遇到独立的 `&&` / `||` / `|` / `;` / `&` 结束；
//! - 位置参数里的下载链接：同一命令多个 URI 是同一文件的镜像，只取第一个；`-Z` /
//!   `--force-sequential` 时每个 URI 各成一条；磁力 / ed2k / thunder 不是镜像，各成一条；
//! - `-o` / `--out`（仅单条）→ 文件名，`--checksum`（仅单条、引擎支持的算法且哈希合法）→ 校验；
//! - `--header` / `--referer` / `-U` / `--user-agent` 收集为 [`RequestHints`]，由表单回填高级字段（`Referer` 进「来源页」专门字段，不再作自定义请求头）；
//! - 其余选项（含带值的 `-d` / `-x` / `-s` / `-k` / `--load-cookies` 等）安全忽略，其值不会被当成链接；
//! - `-i` / `--input-file` 读取本地文件，无法处理 → 该命令不产出条目。

use std::borrow::Cow;

use super::new_download::{UrlEntry, starts_with_link};

/// 命令里能映射到表单高级字段的请求信息。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RequestHints {
    /// `Cookie` 头（多个 `Cookie` 头以 `; ` 连接）。
    pub(crate) cookie: String,
    /// `-U` / `--user-agent` / `User-Agent` 头（后出现者生效）。
    pub(crate) user_agent: String,
    /// `--referer` / `Referer` 头（后出现者生效），回填「来源页」框。
    pub(crate) referrer: String,
    /// 其余请求头，按首次出现顺序，同名（忽略大小写）后者覆盖前者。
    pub(crate) headers: Vec<(String, String)>,
}

impl RequestHints {
    fn set_header(&mut self, name: &str, value: &str) {
        match self
            .headers
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            Some((_, existing)) => value.clone_into(existing),
            None => self.headers.push((name.to_owned(), value.to_owned())),
        }
    }

    /// `--header "Name: value"`；`Cookie` / `User-Agent` 进专门字段，空名 / 空值忽略。
    fn add_raw_header(&mut self, raw: &str) {
        let Some((name, value)) = raw.split_once(':') else {
            return;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() || name.contains(char::is_whitespace) {
            return;
        }
        if name.eq_ignore_ascii_case("cookie") {
            if !self.cookie.is_empty() {
                self.cookie.push_str("; ");
            }
            self.cookie.push_str(value);
        } else if name.eq_ignore_ascii_case("user-agent") {
            value.clone_into(&mut self.user_agent);
        } else if name.eq_ignore_ascii_case("referer") {
            value.clone_into(&mut self.referrer);
        } else {
            self.set_header(name, value);
        }
    }
}

/// 一条 aria2c 命令的解析结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Aria2cCommand {
    pub(crate) entries: Vec<UrlEntry>,
    pub(crate) hints: RequestHints,
}

/// 需要独立取值的长选项（`--name value`）；`--name=value` 写法不查此表。
/// 不在表内的长选项视为无值开关（aria2 的布尔选项只接受 `=` 写法）。
const LONG_VALUE_OPTIONS: &[&str] = &[
    "all-proxy",
    "all-proxy-passwd",
    "all-proxy-user",
    "async-dns-server",
    "auto-save-interval",
    "bt-exclude-tracker",
    "bt-external-ip",
    "bt-max-open-files",
    "bt-max-peers",
    "bt-min-crypto-level",
    "bt-prioritize-piece",
    "bt-request-peer-speed-limit",
    "bt-stop-timeout",
    "bt-tracker",
    "bt-tracker-connect-timeout",
    "bt-tracker-interval",
    "bt-tracker-timeout",
    "ca-certificate",
    "certificate",
    "checksum",
    "conf-path",
    "connect-timeout",
    "console-log-level",
    "dht-file-path",
    "dht-file-path6",
    "dht-listen-addr6",
    "dht-listen-port",
    "dht-message-timeout",
    "dir",
    "disk-cache",
    "download-result",
    "dscp",
    "event-poll",
    "file-allocation",
    "follow-metalink",
    "follow-torrent",
    "ftp-passwd",
    "ftp-proxy",
    "ftp-proxy-passwd",
    "ftp-proxy-user",
    "ftp-type",
    "ftp-user",
    "gid",
    "header",
    "http-passwd",
    "http-proxy",
    "http-proxy-passwd",
    "http-proxy-user",
    "http-user",
    "https-proxy",
    "https-proxy-passwd",
    "https-proxy-user",
    "index-out",
    "input-file",
    "interface",
    "listen-port",
    "load-cookies",
    "log",
    "log-level",
    "lowest-speed-limit",
    "max-concurrent-downloads",
    "max-connection-per-server",
    "max-download-limit",
    "max-download-result",
    "max-file-not-found",
    "max-mmap-limit",
    "max-overall-download-limit",
    "max-overall-upload-limit",
    "max-resume-failure-tries",
    "max-tries",
    "max-upload-limit",
    "metalink-base-uri",
    "metalink-file",
    "metalink-language",
    "metalink-location",
    "metalink-os",
    "metalink-preferred-protocol",
    "metalink-version",
    "min-split-size",
    "min-tls-version",
    "multiple-interface",
    "netrc-path",
    "no-file-allocation-limit",
    "no-proxy",
    "on-bt-download-complete",
    "on-download-complete",
    "on-download-error",
    "on-download-pause",
    "on-download-start",
    "on-download-stop",
    "out",
    "peer-agent",
    "peer-id-prefix",
    "piece-length",
    "private-key",
    "proxy-method",
    "referer",
    "retry-wait",
    "rlimit-nofile",
    "rpc-certificate",
    "rpc-listen-port",
    "rpc-max-request-size",
    "rpc-passwd",
    "rpc-private-key",
    "rpc-secret",
    "rpc-user",
    "save-cookies",
    "save-session",
    "save-session-interval",
    "seed-ratio",
    "seed-time",
    "select-file",
    "server-stat-if",
    "server-stat-of",
    "server-stat-timeout",
    "socket-recv-buffer-size",
    "split",
    "stop",
    "stop-with-process",
    "stream-piece-selector",
    "summary-interval",
    "timeout",
    "torrent-file",
    "uri-selector",
    "user-agent",
];

/// 带值短选项（`-x16` / `-x 16`）：`-d -i -j -k -l -m -M -o -O -s -t -T -u -U -x`。
const SHORT_VALUE_OPTIONS: &str = "dijklmMoOstTuUx";

/// 命令行的参数部分（去掉命令名之后）；该行不是 aria2c 命令时为 `None`。
///
/// 命令名必须位于行首（与链接框「行首空白 = 选项行」的规则一致），可带路径 / 引号
/// （`"C:\Program Files\aria2\aria2c.exe"`）；含 `://` 的链接行不会被误判。
fn command_args(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let first = line.chars().next()?;
    if first.is_whitespace() {
        return None;
    }
    let (head, rest) = if first == '"' || first == '\'' {
        let body = &line[1..];
        let end = body.find(first)?;
        (&body[..end], &body[end + 1..])
    } else {
        let end = line.find(char::is_whitespace).unwrap_or(line.len());
        line.split_at(end)
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    if head.contains("://") {
        return None;
    }
    let name = head.rsplit(['/', '\\']).next().unwrap_or(head);
    (name.eq_ignore_ascii_case("aria2c") || name.eq_ignore_ascii_case("aria2c.exe")).then_some(rest)
}

/// 行末的续行符（奇数个 `\` 或 Windows `^`）：返回去掉续行符后的内容。
fn strip_continuation(line: &str) -> Option<&str> {
    let marker = line
        .chars()
        .next_back()
        .filter(|c| matches!(c, '\\' | '^'))?;
    let run = line.chars().rev().take_while(|c| *c == marker).count();
    (run % 2 == 1).then(|| &line[..line.len() - 1])
}

/// 合并 aria2c 命令的续行（`\`+换行、Windows `^`+换行），只作用于 aria2c 命令内部；
/// 其余行逐字保留。必须先于按行解析进行：续行后的缩进行否则会被当作选项行丢弃。
pub(crate) fn join_continuations(text: &str) -> Cow<'_, str> {
    let needs_join = text
        .split('\n')
        .any(|line| command_args(line).is_some() && strip_continuation(line.trim_end()).is_some());
    if !needs_join {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split('\n');
    let mut first = true;
    while let Some(line) = lines.next() {
        if !first {
            out.push('\n');
        }
        first = false;
        if command_args(line).is_none() {
            out.push_str(line);
            continue;
        }
        let mut piece = line;
        loop {
            let trimmed = piece.trim_end();
            let Some(stripped) = strip_continuation(trimmed) else {
                out.push_str(trimmed);
                break;
            };
            out.push_str(stripped);
            match lines.next() {
                Some(next) => piece = next,
                None => break,
            }
        }
    }
    Cow::Owned(out)
}

/// 切词结果；`literal` = 含引号 / 转义（此时 `&&` 等不是 shell 运算符）。
struct Token {
    text: String,
    literal: bool,
}

/// 按 POSIX shell 规则切词：单引号内逐字，双引号内只转义 `"` `\` `$` `` ` ``，
/// 引号外 `\` 转义下一个字符；引号未闭合时取到末尾。
fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut in_token = false;
    let mut literal = false;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(Token {
                        text: std::mem::take(&mut text),
                        literal,
                    });
                    in_token = false;
                    literal = false;
                }
            }
            '\'' => {
                in_token = true;
                literal = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    text.push(c);
                }
            }
            '"' => {
                in_token = true;
                literal = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.clone().next() {
                            Some(next @ ('"' | '\\' | '$' | '`')) => {
                                text.push(next);
                                chars.next();
                            }
                            _ => text.push('\\'),
                        },
                        c => text.push(c),
                    }
                }
            }
            '\\' => {
                in_token = true;
                literal = true;
                if let Some(next) = chars.next() {
                    text.push(next);
                }
            }
            c => {
                in_token = true;
                text.push(c);
            }
        }
    }
    if in_token {
        tokens.push(Token { text, literal });
    }
    tokens
}

/// 扫描过程中收集的命令状态。
#[derive(Default)]
struct Scan {
    uris: Vec<String>,
    out: Option<String>,
    checksum: Option<String>,
    force_sequential: bool,
    input_file: bool,
    hints: RequestHints,
}

impl Scan {
    fn positional(&mut self, text: &str) {
        if starts_with_link(text) {
            self.uris.push(text.to_owned());
        }
    }

    fn long_option(&mut self, name: &str, value: Option<&str>) {
        match name {
            "out" => self.out = value.map(out_file_name),
            "checksum" => self.checksum = value.and_then(normalize_checksum),
            "header" => {
                if let Some(value) = value {
                    self.hints.add_raw_header(value);
                }
            }
            "referer" => self.set_referer(value),
            "user-agent" => self.set_user_agent(value),
            "force-sequential" => {
                self.force_sequential = value.is_none_or(|v| !v.eq_ignore_ascii_case("false"));
            }
            "input-file" => self.input_file = true,
            _ => {}
        }
    }

    fn short_option(&mut self, flag: char, value: Option<&str>) {
        match flag {
            'o' => self.out = value.map(out_file_name),
            'U' => self.set_user_agent(value),
            'i' => self.input_file = true,
            _ => {}
        }
    }

    fn set_referer(&mut self, value: Option<&str>) {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            value.clone_into(&mut self.hints.referrer);
        }
    }

    fn set_user_agent(&mut self, value: Option<&str>) {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            value.clone_into(&mut self.hints.user_agent);
        }
    }

    fn finish(self) -> Aria2cCommand {
        if self.input_file {
            return Aria2cCommand::default();
        }
        let mut entries = Vec::with_capacity(self.uris.len());
        let mut mirror_taken = false;
        for uri in self.uris {
            if !self.force_sequential && is_mirrorable(&uri) {
                if mirror_taken {
                    continue;
                }
                mirror_taken = true;
            }
            entries.push(UrlEntry {
                url: uri,
                ..UrlEntry::default()
            });
        }
        if let [entry] = entries.as_mut_slice() {
            if let Some(out) = self.out.filter(|out| !out.is_empty()) {
                entry.file_name = out;
            }
            if let Some(checksum) = self.checksum {
                entry.checksum = checksum;
            }
        }
        Aria2cCommand {
            entries,
            hints: self.hints,
        }
    }
}

/// http(s) / ftp 才可能是同一文件的镜像；磁力 / ed2k / thunder 自成一个下载。
fn is_mirrorable(uri: &str) -> bool {
    ["http://", "https://", "ftp://"].into_iter().any(|scheme| {
        uri.len() >= scheme.len()
            && uri.as_bytes()[..scheme.len()].eq_ignore_ascii_case(scheme.as_bytes())
    })
}

/// `-o` 值 → 文件名：只取最后一段（aria2 允许 `sub/dir/name`，引擎文件名不含路径）；
/// `.` / `..` 无效。
fn out_file_name(raw: &str) -> String {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    if matches!(name, "" | "." | "..") {
        String::new()
    } else {
        name.to_owned()
    }
}

/// `--checksum=TYPE=DIGEST` → `algo=hex`（小写）。只接受引擎支持的算法
/// （md5 / sha-1 / sha-256 / sha-512）且哈希长度合法：畸形校验值会让引擎拒绝建任务。
fn normalize_checksum(raw: &str) -> Option<String> {
    let (algo, digest) = raw.split_once('=')?;
    let algo = algo.trim().to_ascii_lowercase();
    let digest = digest.trim().to_ascii_lowercase();
    let len = match algo.as_str() {
        "md5" => 32,
        "sha-1" | "sha1" => 40,
        "sha-256" | "sha256" => 64,
        "sha-512" | "sha512" => 128,
        _ => return None,
    };
    (digest.len() == len && digest.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{algo}={digest}"))
}

/// 解析一行 aria2c 命令（调用方已合并续行）；不是 aria2c 命令时返回 `None`。
pub(crate) fn parse_command(line: &str) -> Option<Aria2cCommand> {
    let args = command_args(line)?;
    let tokens = tokenize(args);
    let mut scan = Scan::default();
    let mut options_done = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;
        let text = token.text.as_str();
        if !token.literal && matches!(text, "&&" | "||" | "|" | ";" | "&") {
            break;
        }
        if options_done || !text.starts_with('-') || text == "-" {
            scan.positional(text);
        } else if text == "--" {
            options_done = true;
        } else if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let value = match inline {
                Some(value) => Some(value),
                None if LONG_VALUE_OPTIONS.contains(&name) => {
                    let next = tokens.get(index).map(|token| token.text.as_str());
                    index += usize::from(next.is_some());
                    next
                }
                None => None,
            };
            scan.long_option(name, value);
        } else {
            let cluster = &text[1..];
            for (offset, flag) in cluster.char_indices() {
                if SHORT_VALUE_OPTIONS.contains(flag) {
                    let attached = &cluster[offset + flag.len_utf8()..];
                    let value = if attached.is_empty() {
                        let next = tokens.get(index).map(|token| token.text.as_str());
                        index += usize::from(next.is_some());
                        next
                    } else {
                        Some(attached)
                    };
                    scan.short_option(flag, value);
                    break;
                }
                if flag == 'Z' {
                    scan.force_sequential = true;
                }
            }
        }
    }
    Some(scan.finish())
}

/// 回填单个表单字段的决策（Cookie / UA / 请求头值）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FieldFill {
    /// 不动。
    Keep,
    /// 用户已手动改过：不动，且此后不再认领该字段（忘记上次回填值）。
    Forget,
    /// 写入新值（并记录为回填值）。
    Set(String),
    /// 命令里不再带该项：清空我们回填过的值（并忘记回填值）。
    Clear,
}

/// 命令头部信息 → 字段的回填规则：只在字段为空、或仍是上次自动回填的值（用户没碰过，
/// 命令编辑中随之更新）时才写；用户填过的内容绝不覆盖。`filled` = 上次回填的值。
pub(crate) fn plan_field_fill(current: &str, filled: Option<&str>, hint: &str) -> FieldFill {
    let current = current.trim();
    if !current.is_empty() && filled != Some(current) {
        return if filled.is_some() {
            FieldFill::Forget
        } else {
            FieldFill::Keep
        };
    }
    match (hint.is_empty(), filled.is_some()) {
        (true, true) => FieldFill::Clear,
        (true, false) => FieldFill::Keep,
        (false, _) if current == hint => FieldFill::Keep,
        (false, _) => FieldFill::Set(hint.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{RequestHints, join_continuations, parse_command};
    use crate::model::new_download::{UrlEntry, parse_entries};

    fn urls(command: &str) -> Vec<String> {
        parse_command(command)
            .expect("aria2c command")
            .entries
            .into_iter()
            .map(|entry| entry.url)
            .collect()
    }

    fn entry(url: &str) -> UrlEntry {
        UrlEntry {
            url: url.to_owned(),
            ..UrlEntry::default()
        }
    }

    #[test]
    fn only_aria2c_command_lines_are_recognized() {
        for line in [
            "aria2c https://a.example/x",
            "ARIA2C https://a.example/x",
            "aria2c.exe https://a.example/x",
            "/usr/local/bin/aria2c https://a.example/x",
            r#""C:\Program Files\aria2\aria2c.exe" https://a.example/x"#,
            r"C:\aria2\aria2c.exe https://a.example/x",
        ] {
            assert_eq!(urls(line), ["https://a.example/x"], "{line}");
        }
        for line in [
            "https://a.example/aria2c https://b.example/x",
            "ed2k://|file|aria2c|1|ff|/",
            "aria2c-wrapper https://a.example/x",
            "  aria2c https://a.example/x",
            "see aria2c https://a.example/x",
            "aria2",
        ] {
            assert!(parse_command(line).is_none(), "{line}");
        }
    }

    #[test]
    fn quoting_and_escapes_follow_shell_rules() {
        assert_eq!(
            urls(
                r#"aria2c 'https://a.example/x?a=1&b=2' "https://b.example/y z?q=\"1\"" https://c.example/a\ b"#
            ),
            ["https://a.example/x?a=1&b=2"]
        );
        let command = parse_command(
            r#"aria2c -Z 'https://a.example/x?a=1&b=2' "https://b.example/y z" https://c.example/a\ b "https://d.example/\$x""#,
        )
        .expect("command");
        let urls: Vec<_> = command.entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://a.example/x?a=1&b=2",
                "https://b.example/y z",
                "https://c.example/a b",
                "https://d.example/$x",
            ]
        );
        // 双引号内非特殊字符前的反斜杠保留（Windows 路径）。
        let command = parse_command(r#"aria2c -d "C:\Downloads" -o "a b.zip" https://a.example/x"#)
            .expect("command");
        assert_eq!(command.entries[0].file_name, "a b.zip");
    }

    #[test]
    fn out_forms_and_checksum() {
        let digest = "d".repeat(64);
        let command = parse_command(&format!(
            "aria2c --out=a.zip --checksum=SHA-256={} https://a.example/x",
            digest.to_uppercase()
        ))
        .expect("command");
        assert_eq!(command.entries[0].file_name, "a.zip");
        assert_eq!(command.entries[0].checksum, format!("sha-256={digest}"));
        for line in [
            "aria2c --out b.zip https://a.example/x",
            "aria2c -o b.zip https://a.example/x",
            "aria2c -ob.zip https://a.example/x",
            "aria2c -cx16 -o b.zip https://a.example/x",
            "aria2c https://a.example/x -o b.zip",
        ] {
            assert_eq!(
                parse_command(line).expect("command").entries[0].file_name,
                "b.zip",
                "{line}"
            );
        }
        // 路径只取最后一段；畸形 / 不支持的校验被丢弃（引擎会拒绝畸形校验值）。
        let command = parse_command(
            "aria2c -o sub/dir/c.zip --checksum=adler32=abcd1234 https://a.example/x",
        )
        .expect("command");
        assert_eq!(command.entries[0].file_name, "c.zip");
        assert_eq!(command.entries[0].checksum, "");
        assert_eq!(
            parse_command("aria2c -o .. --checksum=md5=xyz https://a.example/x")
                .expect("command")
                .entries[0],
            entry("https://a.example/x")
        );
    }

    #[test]
    fn multiple_uris_are_mirrors_unless_forced_sequential() {
        assert_eq!(
            urls("aria2c -o x.zip https://a.example/x https://b.example/x ftp://c.example/x"),
            ["https://a.example/x"]
        );
        let sequential =
            parse_command("aria2c -Z -o ignored.zip https://a.example/x https://b.example/y")
                .expect("command");
        assert_eq!(
            sequential.entries,
            [entry("https://a.example/x"), entry("https://b.example/y")]
        );
        for flag in ["--force-sequential", "--force-sequential=true", "-cZ"] {
            assert_eq!(
                urls(&format!(
                    "aria2c {flag} https://a.example/x https://b.example/y"
                ))
                .len(),
                2,
                "{flag}"
            );
        }
        assert_eq!(
            urls("aria2c --force-sequential=false https://a.example/x https://b.example/y").len(),
            1
        );
        // 磁力不是镜像：各成一条；镜像规则只折叠 http(s)/ftp。
        assert_eq!(
            urls(
                "aria2c 'magnet:?xt=urn:btih:aa' https://a.example/x https://b.example/x 'magnet:?xt=urn:btih:bb'"
            ),
            [
                "magnet:?xt=urn:btih:aa",
                "https://a.example/x",
                "magnet:?xt=urn:btih:bb"
            ]
        );
    }

    #[test]
    fn value_options_never_leak_values_into_positionals() {
        let command = parse_command(
            "aria2c -d /tmp/dl -x 16 -s16 -k 1M --load-cookies cookies.txt --all-proxy http://proxy:8080 \
             --referer https://ref.example/ --conf-path=/etc/a.conf --continue true https://a.example/x",
        )
        .expect("command");
        assert_eq!(command.entries, [entry("https://a.example/x")]);
        assert_eq!(command.hints.referrer, "https://ref.example/");
        assert!(command.hints.headers.is_empty());
        // `--` 之后全是位置参数。
        assert_eq!(
            urls("aria2c -- https://a.example/x"),
            ["https://a.example/x"]
        );
        // 运算符之后不属于该命令。
        assert_eq!(
            urls("aria2c https://a.example/x && echo https://b.example/y"),
            ["https://a.example/x"]
        );
        assert_eq!(
            urls("aria2c https://a.example/x?a=1&b=2"),
            ["https://a.example/x?a=1&b=2"]
        );
    }

    #[test]
    fn headers_are_collected_into_hints() {
        let command = parse_command(
            r#"aria2c --header "Cookie: a=1" --header='X-Token: t' --header "cookie: b=2" --header "Referer: https://r.example/" -U "Agent/1.0" --header "User-Agent: Agent/2.0" --header "Bad" --header "X-Token: t2" https://a.example/x"#,
        )
        .expect("command");
        assert_eq!(
            command.hints,
            RequestHints {
                cookie: "a=1; b=2".to_owned(),
                user_agent: "Agent/2.0".to_owned(),
                referrer: "https://r.example/".to_owned(),
                headers: vec![("X-Token".to_owned(), "t2".to_owned())],
            }
        );
        assert_eq!(
            parse_command("aria2c https://a.example/x")
                .expect("command")
                .hints,
            RequestHints::default()
        );
    }

    #[test]
    fn input_file_commands_yield_nothing() {
        for line in [
            "aria2c -i urls.txt",
            "aria2c --input-file=urls.txt https://a.example/x",
            "aria2c -x16 -i list.txt --header 'Cookie: a=1'",
        ] {
            assert_eq!(
                parse_command(line).expect("command"),
                super::Aria2cCommand::default(),
                "{line}"
            );
        }
    }

    #[test]
    fn continuations_merge_only_inside_aria2c_commands() {
        let text = "aria2c -x16 \\\n  -o a.zip \\\r\n  https://a.example/x\nhttps://b.example/y\n  out=b.zip";
        assert_eq!(
            join_continuations(text),
            "aria2c -x16   -o a.zip   https://a.example/x\nhttps://b.example/y\n  out=b.zip"
        );
        // Windows `^` 续行；`^^` / `\\` 是字面字符，不续行。
        assert_eq!(
            join_continuations("aria2c ^\n  -o a.zip ^\n  https://a.example/x"),
            "aria2c   -o a.zip   https://a.example/x"
        );
        let literal = "aria2c https://a.example/x\\\\\nhttps://b.example/y";
        assert_eq!(join_continuations(literal), literal);
        // 非 aria2c 行末尾的反斜杠逐字保留。
        let plain = "https://a.example/x\\\n  out=a.zip";
        assert_eq!(join_continuations(plain), plain);
    }

    #[test]
    fn parse_entries_integrates_commands_and_keeps_other_lines() {
        let text = "https://plain.example/a.zip\n  out=plain.zip\naria2c -x16 \\\n  -o b.zip \\\n  'https://b.example/b?x=1&y=2'\n  checksum=md5=ff\nmagnet:?xt=urn:btih:xyz\naria2c -i list.txt\n  out=ignored\nhttps://tail.example/t";
        for loose in [false, true] {
            let entries = parse_entries(text, loose);
            assert_eq!(
                entries,
                [
                    UrlEntry {
                        url: "https://plain.example/a.zip".to_owned(),
                        file_name: "plain.zip".to_owned(),
                        ..UrlEntry::default()
                    },
                    UrlEntry {
                        url: "https://b.example/b?x=1&y=2".to_owned(),
                        file_name: "b.zip".to_owned(),
                        checksum: "md5=ff".to_owned(),
                    },
                    entry("magnet:?xt=urn:btih:xyz"),
                    entry("https://tail.example/t"),
                ],
                "loose={loose}"
            );
        }
        // -Z 的多条与随后的选项行：选项行附着到最后一条。
        let entries = parse_entries(
            "aria2c -Z https://a.example/1 https://a.example/2\n  out=two.zip",
            false,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].file_name, "");
        assert_eq!(entries[1].file_name, "two.zip");
    }

    #[test]
    fn field_fill_only_touches_empty_or_self_filled_fields() {
        use super::{FieldFill, plan_field_fill};
        // 空字段：写入；命令没带：不动。
        assert_eq!(
            plan_field_fill("", None, "a=1"),
            FieldFill::Set("a=1".to_owned())
        );
        assert_eq!(plan_field_fill("  ", None, ""), FieldFill::Keep);
        // 用户自己填的：不覆盖、不清空。
        assert_eq!(plan_field_fill("mine", None, "a=1"), FieldFill::Keep);
        assert_eq!(plan_field_fill("mine", None, ""), FieldFill::Keep);
        assert_eq!(plan_field_fill("a=1", None, "a=1"), FieldFill::Keep);
        // 自动回填的值：随命令更新 / 清除。
        assert_eq!(
            plan_field_fill("a", Some("a"), "ab"),
            FieldFill::Set("ab".to_owned())
        );
        assert_eq!(plan_field_fill("ab", Some("ab"), "ab"), FieldFill::Keep);
        assert_eq!(plan_field_fill("ab", Some("ab"), ""), FieldFill::Clear);
        // 回填后用户改过：此后不再认领。
        assert_eq!(
            plan_field_fill("edited", Some("ab"), "abc"),
            FieldFill::Forget
        );
        // 回填后用户清空：命令再变化时可重新回填。
        assert_eq!(
            plan_field_fill("", Some("ab"), "abc"),
            FieldFill::Set("abc".to_owned())
        );
    }
}
