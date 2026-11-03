//! dufs 的嵌入、释放与进程管理

use crate::config::Config;
pub use crate::job::JobHandle;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// dufs 二进制直接嵌进主程序。改这个文件时记得同步 DUFS_VERSION。
static DUFS_EXE: &[u8] = include_bytes!("../assets/dufs.exe");

/// VC++ 运行库（x64），随主程序一起嵌入，释放到 dufs.exe 旁边。
///
/// ★ 为什么必须带上它：
///   dufs 官方发布的 Windows 版本是**动态链接 CRT** 的，它的导入表里有
///   `VCRUNTIME140.dll`。而这个 DLL **不是 Windows 自带的** —— 它来自
///   “Microsoft Visual C++ 2015-2022 Redistributable”。
///   在一台全新安装、没装过任何 VC++ 程序的 Windows 上，
///   释放出来的 dufs.exe 会因为找不到这个 DLL 而**弹系统报错框、根本起不来**，
///   用户看到的只是「服务启动失败」，完全无从下手。
///   把 DLL 放在 dufs.exe 同目录（app-local 部署）就能解决：
///   Windows 加载器搜索顺序里，exe 所在目录优先于 System32。
///
/// 文件来源：开发机 `C:\Windows\System32\vcruntime140.dll`（14.50.35719.0）。
/// 它是微软允许随应用一起分发的可再发行组件。升级时可以换成更新的 14.x 版本，
/// VCRUNTIME140 在 v14 系列内 ABI 兼容。
static VCRUNTIME140: &[u8] = include_bytes!("../assets/vcruntime140.dll");

/// 释放到 dufs 旁边的运行库文件名（不能改，加载器按名字找）
const VCRUNTIME140_NAME: &str = "vcruntime140.dll";

/// 与 assets/dufs.exe 对应的版本号。用于生成带版本的文件名，
/// 这样升级时不会撞上"旧版本正被占用"的问题。
pub const DUFS_VERSION: &str = "0.46.0";

/// 捕获到的 dufs 输出（环形缓冲，只保留最后若干行，用于报错）
pub type LogSink = Arc<Mutex<VecDeque<String>>>;
const LOG_CAP: usize = 200;

/// dufs 释放目录：%LOCALAPPDATA%\dufs-gui\bin
///
/// 为什么不用 %TEMP%：
///   1. 防火墙规则可以绑定到这个固定路径，用户只需授权一次；
///   2. 固定路径 + 固定文件名，杀软的"临时目录释放可执行文件"特征不会命中；
///   3. 只需在第一次运行时写 4.8MB，之后启动秒开。
pub fn bin_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("dufs-gui").join("bin")
}

fn target_path() -> PathBuf {
    bin_dir().join(format!("dufs-{DUFS_VERSION}.exe"))
}

/// 确保 dufs.exe 已释放到磁盘，返回可执行文件路径。
///
/// 同时会把 VC++ 运行库（`vcruntime140.dll`）释放到同一个目录 ——
/// dufs 依赖它，而 Windows 不自带，详见 `VCRUNTIME140` 的注释。
pub fn ensure_dufs() -> Result<PathBuf, String> {
    // 编译期断言：嵌入的字节数和预期不符说明 assets 被换过，版本号需要一起改
    debug_assert!(
        DUFS_EXE.len() > 1_000_000,
        "内嵌的 dufs.exe 看起来不对（只有 {} 字节）",
        DUFS_EXE.len()
    );

    let dir = bin_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| crate::i18n::tn(crate::i18n::K::ExtractDirFailed, &[&dir.display(), &e]))?;

    let target = target_path();

    // 运行库和 dufs 是两个独立文件，各写各的：
    // 即使 dufs 已经释放过（走了下面的提前返回），也要确保 DLL 在。
    // 尺寸一致就跳过，所以正常启动只 stat 两次，不重写。
    ensure_asset(&dir, VCRUNTIME140_NAME, VCRUNTIME140)?;

    // 已经释放过且大小一致 -> 直接复用
    if let Ok(meta) = std::fs::metadata(&target)
        && meta.len() == DUFS_EXE.len() as u64
    {
        cleanup_old_versions(&dir, &target);
        return Ok(target);
    }

    // 原子落地：先写临时文件再 rename，避免出现"写了一半"的 exe
    let tmp = target.with_extension("exe.tmp");
    std::fs::write(&tmp, DUFS_EXE).map_err(|e| {
        crate::i18n::tn(crate::i18n::K::ExtractWriteFailed, &[&tmp.display(), &e])
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }

    match std::fs::rename(&tmp, &target) {
        Ok(()) => {}
        Err(_) => {
            // 极少数情况：目标文件仍被正在运行的旧进程占用 -> 退化成带 pid 的唯一文件名
            let alt = dir.join(format!("dufs-{DUFS_VERSION}-{}.exe", std::process::id()));
            std::fs::rename(&tmp, &alt)
            .map_err(|e| crate::i18n::tn(crate::i18n::K::ExtractDufsFailed, &[&e]))?;
            let _ = std::fs::remove_file(&tmp);
            return Ok(alt);
        }
    }

    cleanup_old_versions(&dir, &target);
    Ok(target)
}

/// 把一个内嵌资源释放到 `dir` 下（尺寸一致就跳过）。
///
/// 先写 `.tmp` 再 rename，避免其它进程读到半个文件。
fn ensure_asset(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let target = dir.join(name);
    if let Ok(meta) = std::fs::metadata(&target)
        && meta.len() == bytes.len() as u64
    {
        return Ok(());
    }

    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes).map_err(|e| {
        crate::i18n::tn(crate::i18n::K::ExtractWriteFailed, &[&tmp.display(), &e])
    })?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        crate::i18n::tn(crate::i18n::K::ExtractAssetFailed, &[&name, &e])
    })?;
    vlog!("已释放 {name} -> {}", target.display());
    Ok(())
}

/// 清理同目录下的旧版本文件（删除失败就忽略 —— 可能正被别的实例占用）
fn cleanup_old_versions(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
        if (name.starts_with("dufs-") && name.ends_with(".exe")) || name.ends_with(".exe.tmp") {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 拼装 dufs 命令行参数
///
/// 对应关系：共享页 -> serve-path + -b/-p、--allow-*、-a，
/// 进阶页 -> --render-*/--tls-*/--log-*/--hidden/--path-prefix/--enable-cors，
/// 最后是位置参数 serve-path。
/// 把可视化账号列表拼成 `-a` 的值（一个账号一个，不含 `-a` 本体）。
///
/// 同一个（用户名，密码）写了多行 = 它的多个路径，合并成**一个** `-a`，
/// 路径用逗号连起来。用户名为空 = 匿名账号（`@/...`）。
/// 输出顺序 = 各账号首次出现的顺序（稳定、可重现，黄金用例就靠它）。
///
/// 调用方保证已经校验过（`config::validate_accounts` 在启动前跑过）；
/// 这里对坏行只做最基本的跳过，不报错 —— 报错是校验的事，拼装只管拼。
pub fn build_accounts_args(rows: &[crate::config::AccountRow]) -> Vec<String> {
    // (用户名, 密码) -> 它在 out 里的下标
    let mut order: Vec<(String, String)> = Vec::new();
    let mut paths: Vec<Vec<String>> = Vec::new();

    for r in rows {
        let name = r.name.trim().to_string();
        let secret = r.secret.clone();
        let path = r.path.trim();
        if path.is_empty() {
            continue; // 空行（校验时跳过，这里再防一手）
        }
        // 权限只从枚举取 —— 界面文案怎么翻都影响不到这里。
        let entry = format!("{path}:{}", r.perm.as_dufs());

        match order.iter().position(|(n, s)| *n == name && *s == secret) {
            Some(i) => paths[i].push(entry),
            None => {
                order.push((name, secret));
                paths.push(vec![entry]);
            }
        }
    }

    order
        .into_iter()
        .zip(paths)
        .map(|((name, secret), ps)| {
            if name.is_empty() {
                format!("@{}", ps.join(","))
            } else {
                format!("{}:{}@{}", name, secret, ps.join(","))
            }
        })
        .collect()
}

pub fn build_args(cfg: &Config) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // 额外参数原文优先（允许用户自己填没做进界面的选项），
    // 无法区分带空格的引号参数，这点在界面上有说明
    for a in cfg.extra_args.split_whitespace() {
        args.push(a.to_string());
    }

    // ---------- dufs 配置文件 ----------
    // 放在最前面：配置文件里的项会被后面的命令行参数覆盖（实测 CLI 优先）
    if !cfg.dufs_config.trim().is_empty() {
        args.push("-c".into());
        args.push(cfg.dufs_config.trim().to_string());
    }

    // ---------- 监听地址与端口 ----------
    // 空数组 = 所有网卡（0.0.0.0）；非空 = 逐个绑出用户勾选的地址。
    // dufs 的 `-b` 是 append 型，可以重复传，所以多选就是一个地址一个
    // `-b`（实测 `-b 127.0.0.1 -b 10.0.0.20` 两个都绑上）。
    if cfg.bind_ips.is_empty() {
        args.push("-b".into());
        args.push("0.0.0.0".into());
        // ★ 为什么还要补一个 `-b ::`：dufs 裸跑时默认**同时**监听 IPv4 和 IPv6
        //   （`default_addrs()` 在 IPv6 可用时返回 ["0.0.0.0", "::"]）。
        //   我们只传 0.0.0.0 的话，纯 IPv6 环境（部分校园网 / 容器 / 运营商网络）
        //   根本连不上，界面那句「手机通过任意网络都能连」就成了空话。
        //
        // ★ 为什么绝不能无条件加：dufs **只要有一个地址绑不上，整个服务就起不来**。
        //   实测 `-b 10.99.99.99 -b 127.0.0.1` → `Error: Failed to bind` 后进程直接退出，
        //   连 127.0.0.1 也不通。所以在没有 IPv6 的机器上多传 `::` 会把服务搞挂。
        //   先探测（`net::ipv6_available`，由 service 每次启动前写入 cfg）。
        if cfg.ipv6_available {
            args.push("-b".into());
            args.push("::".into());
        }
    } else {
        for ip in &cfg.bind_ips {
            let ip = ip.trim();
            // 空项直接跳过：配置里留下一个空串不该让整个服务起不来
            if ip.is_empty() {
                continue;
            }
            args.push("-b".into());
            args.push(ip.to_string());
        }
    }
    args.push("-p".into());
    args.push(cfg.port.to_string());

    // ---------- URL 前缀 ----------
    if !cfg.path_prefix.trim().is_empty() {
        args.push("--path-prefix".into());
        args.push(cfg.path_prefix.trim().to_string());
    }

    // 单文件模式：dufs 把文件直接挂在 `/` 上。实测上传 / 删除 / 搜索 / 打包 /
    // 隐藏 / 渲染这些对单个文件都没有意义（写操作返回 200 但什么都不做），
    // 传了也是白传。下面直接省略，免得用户以为「勾了允许删除就能删」。
    // 例外是 CORS：它管的是 GET 的响应头，对单文件照样有意义，照常传。
    //
    // ★ 定义放在隐藏块之前：隐藏 / 权限 / 压缩 / 渲染四个位置都要用它。
    let single_file = crate::config::serve_path_kind(&cfg.serve_path)
        == crate::config::ServePathKind::File;

    // ---------- 隐藏路径 ----------
    // ★ 单文件模式下没有目录列表可隐藏，跳过。
    // ★ 传之前先归一化（半角逗号 + 去空白）：见 `norm_hidden` 的两个"静默失效"。
    let hidden = norm_hidden(&cfg.hidden);
    if !single_file && !hidden.is_empty() {
        args.push("--hidden".into());
        args.push(hidden);
    }

    // ---------- 权限 ----------
    // ★ 单文件时整个跳过（single_file 的定义在隐藏块之前）。
    //   注意 CORS 不在这里，它不受影响，照常传。
    if !single_file {
        if cfg.allow_all {
            // -A 已经包含全部允许项，不用再重复传
            args.push("--allow-all".into());
        } else if !cfg.readonly_mode {
            // 只读模式压制所有 allow-*
            if cfg.allow_upload {
                args.push("--allow-upload".into());
            }
            if cfg.allow_search {
                args.push("--allow-search".into());
            }
            if cfg.allow_delete {
                args.push("--allow-delete".into());
            }
            if cfg.allow_archive {
                args.push("--allow-archive".into());
            }
            if cfg.allow_hash {
                args.push("--allow-hash".into());
            }
            if cfg.allow_symlink {
                args.push("--allow-symlink".into());
            }
        }
    }

    // 压缩级别只在能打包下载时才有意义。
    // ★ `--allow-all` 本身也包含打包下载，所以它开着时同样要传 ——
    //   之前这里的条件把 allow_all 排掉了，用户选的压缩级别会被静默丢弃。
    // ★ 单文件模式同样跳过（没有可打包的目录）。
    if !single_file && !cfg.readonly_mode && (cfg.allow_archive || cfg.allow_all) {
        args.push("--compress".into());
        args.push(cfg.compress.as_arg().into());
    }

    // CORS 不是"文件操作权限"，不受只读模式影响
    if cfg.enable_cors {
        args.push("--enable-cors".into());
    }

    // ---------- 自定义前端资源 ----------
    if !cfg.assets.trim().is_empty() {
        args.push("--assets".into());
        args.push(cfg.assets.trim().to_string());
    }

    // ---------- 静态站点模式 ----------
    // ★ 单文件模式下没有目录可渲染，跳过。
    if !single_file && let Some(arg) = cfg.render_mode.as_arg() {
        args.push(arg.into());
    }

    // ---------- 认证 ----------
    // 只有可视化账号列表这一条路（专家模式原文框已删，老规则由
    // `migrate_legacy` 转成列表行）。开着开关就把列表拼成多个 -a，
    // 关着或列表全空就不传 -a（无认证）。
    if cfg.use_auth {
        for rule in build_accounts_args(&cfg.accounts) {
            args.push("-a".into());
            args.push(rule);
        }
    }

    // ---------- HTTPS ----------
    if !cfg.tls_cert.trim().is_empty() && !cfg.tls_key.trim().is_empty() {
        args.push("--tls-cert".into());
        args.push(cfg.tls_cert.trim().to_string());
        args.push("--tls-key".into());
        args.push(cfg.tls_key.trim().to_string());
    }

    // ---------- 日志 ----------
    if !cfg.log_file.trim().is_empty() {
        args.push("--log-file".into());
        args.push(cfg.log_file.trim().to_string());
    }
    if cfg.log_disabled {
        // dufs 用「格式为空」表示不记录 http 日志（实测日志文件会建但内容为空）。
        // 空字符串作为一个独立的 argv 传过去，dufs 能正确收到。
        args.push("--log-format".into());
        args.push(String::new());
    } else if !cfg.log_format.trim().is_empty() {
        args.push("--log-format".into());
        args.push(cfg.log_format.trim().to_string());
    }

    // 位置参数（共享路径：文件夹或单个文件）必须放在最后
    args.push(cfg.serve_path.trim().to_string());

    args
}

/// 「额外命令行参数」里禁止出现的选项：它们只是打印点什么然后退出，
/// 放进真实启动命令里只会让服务起不来（表现为「等待就绪超时」）。
const CLI_ONLY_OPTIONS: [&str; 4] = ["help", "version", "completions", "h"];

/// 归一化 `--hidden` 的值：拆开、去掉每段两端的空白、丢掉空段，
/// 再用**半角逗号**重新拼起来。
///
/// 为什么要做（两个都是**静默失效** —— 规则不生效，但界面上一点提示都没有，
/// 实测 dufs 0.46）：
///   1. **逗号后带空格**：clap 的 `value_delimiter(',')` 不会 trim，
///      `--hidden 'tmp, *.log'` 拆出来是 `tmp` 和 `" *.log"`（带前导空格），
///      于是 `*.log` 这条**永远匹配不上任何东西**。
///   2. **全角逗号**：dufs 只在 ASCII `,` 上拆，`tmp，*.log` 会被当成**一个**
///      模式 —— 同样永不生效。中文输入法下这个太容易打出来了。
///
/// 为什么拆开重拼不会丢信息：文件名里虽然可以带逗号，但 dufs 自己也没有转义机制
/// （它无条件按 `,` 拆），所以带逗号的模式本来就表达不出来。顺带把用户多打的
/// 空格也吃掉了。
fn norm_hidden(raw: &str) -> String {
    raw.split([',', '，'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// dufs 里允许重复出现的选项（实测，clap 的 append 型）：
/// `-b/--bind`、`-a/--auth`、`--hidden`。其它选项重复一次就报错退出。
const REPEATABLE_OPTIONS: [&str; 3] = ["bind", "auth", "hidden"];

/// 把 token 归一成选项名（去掉 `--`、处理 `=`、抹平长短别名）。
/// 不是选项（位置参数、选项的值）则返回 None。
fn option_name(token: &str) -> Option<String> {
    let token = token.trim();
    if !token.starts_with('-') || token.len() < 2 {
        return None;
    }
    // `--port=5000` 这种带等号的写法
    let token = token.split_once('=').map_or(token, |(name, _)| name);

    let long = match token {
        "-b" => "--bind",
        "-p" => "--port",
        "-a" => "--auth",
        "-A" => "--allow-all",
        "-c" => "--config",
        "-h" => "--help",
        "-V" => "--version",
        // 粘连写法 `-p5000`
        t if t.starts_with("-p") && t.len() > 2 => "--port",
        t if t.starts_with("-b") && t.len() > 2 => "--bind",
        t if t.starts_with("-a") && t.len() > 2 => "--auth",
        t if t.starts_with("-c") && t.len() > 2 => "--config",
        t => t,
    };
    Some(long.trim_start_matches('-').to_string())
}

/// 启动前检查拼出来的命令行有没有「同一个选项出现两次」。
///
/// ★ 为什么必须查：clap 默认**不允许**同一个选项出现多次，会直接
/// `error: the argument '--allow-archive' cannot be used multiple times` 退出。
/// 而「进阶 → 额外命令行参数」是原文透传、且拼在最前面，用户很容易把界面上
/// 已经有的选项再抄一遍（界面自己的例子 `--allow-archive --compress high`
/// 恰好就是这种写法）—— 结果就是服务起不来，用户只看到一句英文报错。
///
/// 因为本程序自己不会生成重复项（`-a` 除外），所以一旦查出重复，
/// 必定是「额外命令行参数」造成的。
/// 「额外命令行参数」里查出来的冲突。
///
/// ★ 为什么是结构不是字符串：单测要断言「是不是重复选项」这一类事实，
///   而文案是要翻译的（见 `config::AccountProblem` 里同样的理由）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OptionConflict {
    /// 只输出信息就退出的选项（`--completions` / `-h` / `-V`）
    CliOnly { name: String },
    /// 与界面上的控件重复（dufs 不允许同一选项出现多次）
    Duplicate { name: String },
}

impl OptionConflict {
    /// 给人看的文案。★ **显示层** —— Step 5 起把这些字面量搬进 i18n catalog。
    pub fn describe(&self) -> String {
        use crate::i18n::{K, tn};
        match self {
            OptionConflict::CliOnly { name } => tn(K::OptCliOnly, &[name]),
            // 两处 `{}` 都是同一个选项名，所以传两遍
            OptionConflict::Duplicate { name } => tn(K::OptDuplicate, &[name, name]),
        }

    }
}

impl std::fmt::Display for OptionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// `dufs::start` 的失败原因。
///
/// 分这四类是因为它们的**解法完全不同**（改配置 / 删额外参数 / 装不上的环境问题 / 资源问题），
/// 而且单测需要能在不读文案的前提下断言到具体哪一类。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StartError {
    /// 配置本身不合格（`Config::validate` 的文案）
    Config(String),
    /// 额外命令行参数与界面选项冲突
    Options(OptionConflict),
    /// 进程起不来
    Spawn(String),
    /// 建 Job Object 失败（防孤儿进程的那道保险）
    Job(String),
}

impl StartError {
    /// 给人看的文案（显示层，Step 5 起走 i18n）。
    pub fn describe(&self) -> String {
        use crate::i18n::{K, tn};
        match self {
            StartError::Config(msg) => msg.clone(),
            StartError::Options(c) => c.describe(),
            StartError::Spawn(e) => tn(K::StartSpawnFailed, &[e]),
            StartError::Job(e) => tn(K::StartJobFailed, &[e]),
        }

    }
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

pub fn check_options(args: &[String]) -> Result<(), OptionConflict> {
    let mut seen: Vec<String> = Vec::new();

    for token in args {
        let Some(name) = option_name(token) else { continue };

        if CLI_ONLY_OPTIONS.contains(&name.as_str()) {
            return Err(OptionConflict::CliOnly { name });
        }
        if REPEATABLE_OPTIONS.contains(&name.as_str()) {
            continue; // dufs 允许重复
        }
        if seen.iter().any(|n| n == &name) {
            return Err(OptionConflict::Duplicate { name });
        }
        seen.push(name);
    }
    Ok(())
}

/// 从一组环境变量名里挑出必须清除的（纯函数，方便直接写断言）。
///
/// 判据：名字以 `DUFS_` 开头。
/// ★ Windows 的环境变量名**大小写不敏感**（`dufs_port` 和 `DUFS_PORT` 是同一个），
///   所以先转大写再比；否则用户环境里写个小写变体就漏了。
fn dufs_env_keys<I>(keys: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    keys.into_iter()
        .filter(|k| k.to_string_lossy().to_ascii_uppercase().starts_with("DUFS_"))
        .collect()
}

/// 把子进程环境里所有 `DUFS_*` 变量摘掉。
///
/// ★ 为什么必须做：dufs 支持用环境变量配**所有**选项（官方 README 的
///   「Environment variables」一节；`--help` 里看不到是因为用了 `hide_env`），
///   而 clap 的优先级是「**命令行 > 环境变量**」。
///
///   危险的不是我们显式传的选项，而是那些**因为界面没勾所以压根不传**的：
///   实测本机环境里只要有 `DUFS_ALLOW_DELETE=true`，界面没勾「允许删除」，
///   `DELETE` 却会返回 204 且**文件真的被删掉**；`DUFS_AUTH` 同理会凭空打开认证。
///   表现就是「界面显示与实际不符」，而且极难排查。
///
///   所以启动前统一摘掉，让**界面成为唯一的真相来源**。
///   按**前缀**过滤而不是罗列选项名：dufs 以后新增选项也自动盖住。
fn strip_dufs_env(cmd: &mut Command) {
    let keys = dufs_env_keys(std::env::vars_os().map(|(k, _)| k));
    for key in &keys {
        cmd.env_remove(key);
    }
    if !keys.is_empty() {
        vlog!(
            "已从 dufs 子进程环境里清除 {} 个 DUFS_* 变量: {:?}（否则会覆盖界面设置）",
            keys.len(),
            keys
        );
    }
}

/// 启动 dufs，返回 (子进程, job 句柄, 日志缓冲)
pub fn start(cfg: &Config, exe: &Path) -> Result<(Child, JobHandle, LogSink), StartError> {
    cfg.validate().map_err(StartError::Config)?;

    let args = build_args(cfg);
    // 重复选项会让 dufs 直接退出，提前告诉用户哪里重了
    check_options(&args).map_err(StartError::Options)?;

    let mut cmd = Command::new(exe);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // ★ 必须清掉继承来的 DUFS_*：界面上“没勾”的选项，不能被本机环境变量覆盖
    strip_dufs_env(&mut cmd);

    // Windows：不弹命令行黑框
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| StartError::Spawn(e.to_string()))?;

    // 必须在启动后立刻加入 Job —— 之后任何时刻本进程消失，dufs 都会被连带杀死
    let job =
        JobHandle::create_kill_on_close().map_err(|e| StartError::Job(e.to_string()))?;
    if let Err(e) = job.assign(child.id()) {
        eprintln!("警告：把 dufs 加入 Job Object 失败（{e}），异常退出时可能残留进程");
    }

    // 必须把 stdout/stderr 读干净，否则管道缓冲写满会把 dufs 卡死
    let log: LogSink = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(out) = child.stdout.take() {
        drain(out, log.clone());
    }
    if let Some(err) = child.stderr.take() {
        drain(err, log.clone());
    }

    Ok((child, job, log))
}

fn drain<R: std::io::Read + Send + 'static>(reader: R, sink: LogSink) {
    std::thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let mut buf = sink.lock().expect("cfg lock poisoned");
            if buf.len() >= LOG_CAP {
                buf.pop_front();
            }
            buf.push_back(line);
        }
    });
}

fn log_tail(sink: &LogSink) -> String {
    let buf = sink.lock().expect("cfg lock poisoned");
    buf.iter().cloned().collect::<Vec<_>>().join("\n")
}

/// 等就绪时到底该去连哪个地址。
///
/// ★ 这里曾经固定连 `127.0.0.1`，后果是：用户一旦在「共享」页选了具体网卡
///   （`-b 10.0.0.5`），dufs 只监听那个地址，127.0.0.1 永远连不上，
///   于是 wait_ready 必然超时 -> 界面报「启动失败」并把刚起的 dufs 杀掉。
///   所以必须按实际绑定的地址去探。
fn probe_host(bind: &str) -> String {
    // -b 支持逗号分隔的多个地址（甚至 unix socket），取第一个可探的
    let first = bind.split(',').next().unwrap_or("").trim();
    if first.is_empty()
        || first == "0.0.0.0"
        || first == "::"
        || first == "[::]"
        || first.starts_with('/') // unix socket，Windows 上用不到
    {
        "127.0.0.1".to_string()
    } else {
        first.to_string()
    }
}

/// 等待 dufs 真正可以接受连接。
///
/// 两条判据同时用：
///   a) TCP 能连上 <bind>:port      -> 就绪
///   b) 子进程提前退出            -> 启动失败，把日志带出来
///
/// 不依赖解析 dufs 的 stdout 文本，避免 dufs 改版后判定失效。
pub fn wait_ready(
    child: &mut Child,
    bind: &str,
    port: u16,
    log: &LogSink,
    timeout: Duration,
) -> Result<(), String> {
    let host = probe_host(bind);
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect((host.as_str(), port)).is_ok() {
            return Ok(());
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                let tail = {
                    let t = log_tail(log);
                    if t.is_empty() {
                        crate::i18n::t(crate::i18n::K::StartNoOutput).to_string()
                    } else {
                        t
                    }
                };
                return Err(crate::i18n::tn(
                    crate::i18n::K::StartExitedImmediately,
                    &[&status, &tail],
                ));
            }
            Ok(None) => {}
            Err(e) => {
                return Err(crate::i18n::tn(crate::i18n::K::StartProbeFailed, &[&e]));
            }
        }

        if Instant::now() >= deadline {
            let tail = log_tail(log);
            return Err(crate::i18n::tn(
                crate::i18n::K::StartWaitTimeout,
                &[&timeout.as_secs(), &format!("{host}:{port}"), &tail],
            ));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 一键启动：端口预检 → 释放 exe → 起进程（Job Object 托管）→ 等就绪。
///
/// 纯编排，不碰 UI：在子线程里调，失败直接返回中文错误。
/// 以前住在 `service.rs` 叫 `start_inner`，搬过来名副其实。
pub fn launch(cfg: &Config) -> Result<(Child, JobHandle), String> {
    flog!("启动: {}", build_args(cfg).join(" "));

    // 端口预检：能给出比 dufs 原始报错更友好的信息
    if crate::net::port_in_use(cfg.port) {
        // 区分“被别的程序占了”和“被 Windows 保留了”—— 这两种情况的解法完全不同
        if crate::net::port_reserved(cfg.port) {
            return Err(crate::i18n::tn(
                crate::i18n::K::StartPortReserved,
                &[&cfg.port, &cfg.port.wrapping_add(1000).max(1024)],
            ));
        }
        return Err(crate::i18n::tn(crate::i18n::K::StartPortInUse, &[&cfg.port]));
    }

    // 释放内嵌的 dufs.exe（第一次运行会写约 4.8MB）
    let exe = ensure_dufs()?;
    vlog!("dufs 已释放: {}", exe.display());

    // 启动并托管到 Job Object
    // ★ `start` 的错误是结构化的（单测要断言到具体哪一类），这里落回文案给上层显示
    let (mut child, job, log) = start(cfg, &exe).map_err(|e| e.describe())?;
    let pid = child.id();
    flog!("dufs 已启动 pid={pid}，等待端口就绪…");

    // 等它真正能接受连接
    if let Err(e) = wait_ready(
        &mut child,
        // 勾了多个地址时，用第一个能探的来判断“起没起来”即可；
        // `probe_host` 自己会处理 0.0.0.0 / :: / 空串这些情况
        &cfg.bind_ips.join(","),
        cfg.port,
        &log,
        Duration::from_secs(8),
    ) {
        flog!("等待就绪失败: {e}");
        stop(&mut child); // 启动失败要把半死进程收掉
        return Err(e);
    }

    flog!("dufs 已就绪，监听端口 {}", cfg.port);
    Ok((child, job))
}

/// 停止 dufs：kill + wait。
///
/// 必须 wait() —— 否则子进程会以僵尸（zombie）形式留在句柄表里。
pub fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::dufs_env_keys;
    use std::ffi::OsString;

    /// 前缀过滤：大小写不敏感，且不能误伤看起来像的普通变量
    #[test]
    fn picks_only_dufs_prefixed_keys() {
        let input = [
            "DUFS_PORT",
            "dufs_allow_delete", // Windows 下与 DUFS_ALLOW_DELETE 是同一个，必须捕获
            "DuFs_Auth",
            "PATH",
            "DUFSX",     // 没有下划线，不是 dufs 的变量
            "MY_DUFS_1", // 前缀不在开头
            "DUFS_",     // 只有前缀也得清
        ]
        .map(OsString::from);

        let got = dufs_env_keys(input);
        let want = ["DUFS_PORT", "dufs_allow_delete", "DuFs_Auth", "DUFS_"];
        assert_eq!(got, want.map(OsString::from));
    }
}
