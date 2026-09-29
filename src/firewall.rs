//! 防火墙处理（Windows）
//!
//! 关于"能不能自己检测出被拦截"：
//!   **不能可靠检测**。从本机连自己的局域网 IP 走的是本地回环路径，不经过
//!   入站过滤，所以探测成功并不代表手机能连上。因此这里不做"检测到才提示"，
//!   而是「首次启动主动提示 + 提供一键放行按钮」。
//!
//! 提权方式：
//!   普通权限下 netsh 改不了防火墙策略，所以用 ShellExecuteW("runas") 把
//!   自己再启动一份，带上 `--install-firewall <port>` 参数；那份被提权的实例
//!   执行完 netsh 就退出，不启动界面。

use std::process::Command;

const RULE_PREFIX: &str = "dufs-gui";

fn rule_name(port: u16) -> String {
    format!("{RULE_PREFIX}-{port}")
}

/// `request_rule` 的失败原因。
///
/// ★ 这个函数的结果是**直接显示给用户的**（`callbacks.rs` 把它 `set_hint` 出去），
///   所以要能翻译 —— 结构化的目的是让文案与「出了什么事」分开
///   （见 `config::AccountProblem` 里同样的理由）。
///
/// 注意：firewall 里其它失败（netsh 输出、清理规则）**只进日志**，
/// 而按 §4.6 日志保持中文不翻译，所以那些继续用 `String`。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FirewallError {
    /// UAC 被取消/拒绝，或 ShellExecuteW 直接失败（拿不到提权实例）
    Unauthorized,
    /// 取不到自身 exe 路径（包一层原始错误）
    NoSelfPath(String),
    /// 非 Windows 平台。★ 只在非 Windows 构建里会被构造（那边的分支是 cfg 掉的），
    /// 所以在 Windows 上它是"死变体" —— 精确地在这里放行，免得每个平台各留一份枚举。
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
}

impl FirewallError {
    /// 给人看的文案。★ **显示层** —— Step 5 起把这些字面量搬进 i18n catalog。
    pub fn describe(&self) -> String {
        use crate::i18n::{K, t, tn};
        match self {
            FirewallError::Unauthorized => t(K::FwUnauthorized).to_string(),
            FirewallError::NoSelfPath(e) => tn(K::FwNoSelfPath, &[e]),
            FirewallError::Unsupported => t(K::FwUnsupported).to_string(),
        }
    }
}

impl std::fmt::Display for FirewallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// 请求放行某个端口：弹 UAC，让用户确认。
///
/// 返回 `Ok(())` 表示 UAC 已通过（不代表 netsh 一定成功，那只在提权实例里才知道）。
pub fn request_rule(port: u16) -> Result<(), FirewallError> {
    let exe = std::env::current_exe()
        .map_err(|e| FirewallError::NoSelfPath(e.to_string()))?;

    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let op = HSTRING::from("runas");
        let file = HSTRING::from(exe.to_string_lossy().as_ref());
        let params = HSTRING::from(format!("--install-firewall {port}").as_str());
        let dir = HSTRING::from(
            exe.parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
                .as_str(),
        );

        let result = unsafe {
            ShellExecuteW(None, &op, &file, &params, &dir, SW_HIDE)
        };

        // ShellExecuteW 返回值 <= 32 表示失败；5 通常就是用户点了 UAC 的"否"
        let code = result.0 as isize;
        if code <= 32 {
            return Err(FirewallError::Unauthorized);
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = (exe, port);
        Err(FirewallError::Unsupported)
    }
}

/// 列出系统里所有 `dufs-gui-<端口>` 规则。**不需要管理员权限**（只读）。
///
/// 为什么是「搜字符串」而不是解析字段：
/// netsh 的输出字段名是**跟着系统语言变**的（中文上叫「规则名称:」、
/// 英文上是「Rule Name:」），按字段名解析会随语言失效。
/// 而规则名是我们自己起的（纯 ASCII），所以直接在整份输出里找
/// `dufs-gui-` 后面跟数字的地方即可。
///
/// 实测这个命令要 0.4~0.7 秒，**必须放后台线程**调。
pub fn list_rules() -> Vec<String> {
    let Ok(out) = console_command("netsh")
        .args(["advfirewall", "firewall", "show", "rule", "name=all"])
        .output()
    else {
        return Vec::new();
    };
    parse_rules(&String::from_utf8_lossy(&out.stdout))
}

/// 从 netsh 输出里抽出 `dufs-gui-<数字>` 规则名（去重、排序）。
///
/// 抽成纯函数是为了能拿中/英文两种输出直接写断言 ——
/// 这块逻辑一旦随语言失效，界面上就会“看不到已有规则”。
pub fn parse_rules(text: &str) -> Vec<String> {
    const PREFIX: &str = "dufs-gui-";

    let mut found: Vec<String> = Vec::new();
    let mut rest: &str = text;
    while let Some(pos) = rest.find(PREFIX) {
        let after = &rest[pos + PREFIX.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let name = format!("{PREFIX}{digits}");
            if !found.contains(&name) {
                found.push(name);
            }
        }
        rest = after;
    }
    found.sort();
    found
}

/// 清掉所有 `dufs-gui-*` 规则（删规则要管理员，会弹 UAC）。
pub fn cleanup_rules() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("取不到自身路径: {e}"))?;

    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let op = HSTRING::from("runas");
        let file = HSTRING::from(exe.to_string_lossy().as_ref());
        let params = HSTRING::from("--cleanup-firewall");

        let ret = unsafe {
            ShellExecuteW(
                None,
                &op,
                &file,
                &params,
                None,
                SW_HIDE,
            )
        };
        // ShellExecuteW 返回大于 32 表示成功；32 是「用户取消（UAC 被拒）」
        if ret.0 as usize > 32 {
            Ok(())
        } else if ret.0 as usize == 32 {
            Err(crate::i18n::t(crate::i18n::K::FwUacCancelled).to_string())
        } else {
            Err(crate::i18n::tn(
                crate::i18n::K::FwElevateFailed,
                &[&(ret.0 as usize)],
            ))
        }
    }

    #[cfg(not(windows))]
    {
        let _ = exe;
        Err(crate::i18n::t(crate::i18n::K::FwUnsupported).to_string())
    }
}

/// 提权实例的入口：删掉**所有** `dufs-gui-*` 规则（不只是当前端口）。
///
/// 为什么要删全部：规则名带了端口，用户换过端口就会堆出几条，
/// 而界面上没有办法逐条管理它们。
pub fn elevated_cleanup() -> i32 {
    let names = list_rules();
    if names.is_empty() {
        message_box(
            crate::i18n::t(crate::i18n::K::FwCleanNothing),
            crate::i18n::t(crate::i18n::K::FwCleanCaption),
            false,
        );
        return 0;
    }

    let mut deleted: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for name in &names {
        match run_netsh(&["advfirewall", "firewall", "delete", "rule", &format!("name={name}")])
        {
            Ok(_) => deleted.push(name.clone()),
            Err(_) => failed.push(name.clone()),
        }
    }

    if failed.is_empty() {
        message_box(
            &crate::i18n::tn(
                crate::i18n::K::FwCleaned,
                &[&deleted.len(), &deleted.join(crate::i18n::list_sep())],
            ),
            crate::i18n::t(crate::i18n::K::FwCleanCaption),
            false,
        );
        0
    } else {
        let removed = if deleted.is_empty() {
            crate::i18n::t(crate::i18n::K::ListEmpty).to_string()
        } else {
            deleted.join(crate::i18n::list_sep())
        };
        message_box(
            &crate::i18n::tn(
                crate::i18n::K::FwCleanPartial,
                &[&removed, &failed.join(crate::i18n::list_sep())],
            ),
            crate::i18n::t(crate::i18n::K::FwCleanCaption),
            true,
        );
        1
    }
}

/// 提权实例的入口：真正执行 netsh，然后用弹窗告诉用户结果，返回进程退出码。
pub fn elevated_install(port: u16) -> i32 {
    let name = rule_name(port);

    // 先删再加，保证幂等（重复点不会堆出一堆同名规则）
    let _ = run_netsh(&[
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        &format!("name={name}"),
    ]);

    let add_args = [
        "advfirewall".to_string(),
        "firewall".to_string(),
        "add".to_string(),
        "rule".to_string(),
        format!("name={name}"),
        "dir=in".into(),
        "action=allow".into(),
        "protocol=TCP".into(),
        format!("localport={port}"),
        // ★ 只放行 private/domain，绝不能加 public：
        //   否则在咖啡厅/公司等公共 WiFi 下，整个陌生网络都能访问你的共享目录
        "profile=private,domain".into(),
    ];
    let arg_refs: Vec<&str> = add_args.iter().map(|s| s.as_str()).collect();

    match run_netsh(&arg_refs) {
        Ok(out) => {
            let msg = crate::i18n::tn(crate::i18n::K::FwInstallOk, &[&port, &name]);
            message_box(
                &msg,
                crate::i18n::t(crate::i18n::K::FwInstallCaptionOk),
                false,
            );
            let _ = out;
            0
        }
        Err(e) => {
            let msg = crate::i18n::tn(crate::i18n::K::FwInstallFail, &[&e, &port]);
            message_box(
                &msg,
                crate::i18n::t(crate::i18n::K::FwInstallCaptionFail),
                true,
            );
            1
        }
    }
}

/// 建一个**不弹黑框**的子进程命令。
///
/// ★ 为什么必须包一层：本程序是 GUI 程序（没有控制台），
///   它去启动控制台程序（netsh / cmd）时，Windows 默认会**给子进程新建一个
///   控制台窗口** —— 用户看到的就是「启动或刷新时黑框闪一下」。
///   必须显式加 `CREATE_NO_WINDOW`。
///
/// 这里统一成一个入口，就是为了避免“某处加了、另一处忘了”——
/// `list_rules()` 就是这么漏的（它在启动和刷新时都会跑，所以黑框特别显眼）。
///
/// 现在是 `pub(crate)`：自检（`selftest_options.rs`）里跑 cmd / certutil / curl /
/// openssl 也要用，那几处以前就是漏了的（跑 `--selftest-options` 会闪黑框）。
pub(crate) fn console_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 执行 netsh，返回 (stdout+stderr)
fn run_netsh(args: &[&str]) -> Result<String, String> {
    // 优先用绝对路径，避免 PATH 被裁剪的机器上找不到 netsh
    let netsh = std::env::var("SystemRoot")
        .map(|r| format!("{r}\\System32\\netsh.exe"))
        .ok()
        .filter(|p| std::path::Path::new(p).exists())
        .unwrap_or_else(|| "netsh".to_string());

    let mut cmd = console_command(&netsh);
    cmd.args(args);

    let output = cmd
        .output()
        .map_err(|e| crate::i18n::tn(crate::i18n::K::FwNetshSpawn, &[&netsh, &e]))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if output.status.success() {
        Ok(format!("{stdout}{stderr}"))
    } else {
        Err(crate::i18n::tn(
            crate::i18n::K::FwNetshExit,
            &[&output.status.code().unwrap_or(-1), &stdout, &stderr],
        ))
    }
}

/// 弹出消息框（提权实例里没有界面，用它来告知结果）
pub fn message_box(text: &str, caption: &str, is_error: bool) {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
        };

        let style = if is_error {
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND
        } else {
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND
        };

        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(text),
                &HSTRING::from(caption),
                style,
            );
        }
    }

    #[cfg(not(windows))]
    {
        eprintln!("[{caption}] {text}");
    }
}

/// 用系统默认浏览器打开链接
pub fn open_in_browser(url: &str) {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let op = HSTRING::from("open");
        let file = HSTRING::from(url);
        unsafe {
            ShellExecuteW(None, &op, &file, None, None, SW_SHOWNORMAL);
        }
    }

    #[cfg(not(windows))]
    {
        let _ = url;
    }
}

/// 把主窗口从托盘唤回来，并强制置顶。（仅 Windows）
///
/// 为什么需要这一步：Slint 的 `Window::show()` 只把窗口重新设为可见，
/// 不会把它带到最前面。用户从托盘点「打开界面」时，窗口很可能还压在别的
/// 程序后面，看起来就像"点了没反应"。
///
/// ★ 找窗口按 **PID**（`window_finder`），**不按标题** —— 标题是要翻译的
///   界面文案，按标题找的话英文界面下这里会静默失败（窗口不往前跳）。
#[cfg(windows)]
pub fn focus_main_window() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOMOVE,
        SWP_NOSIZE, SW_RESTORE,
    };

    let Some(raw) = crate::window_finder::main_hwnd() else {
        flog!("找不到主窗口，无法置顶");
        return;
    };
    let hwnd = HWND(raw as *mut core::ffi::c_void);

    unsafe {
        // 最小化的话先恢复
        let _ = ShowWindow(hwnd, SW_RESTORE);

        // SetForegroundWindow 有“前台权限”限制，不是每次都能成功，
        // 所以再用不要求前台权限的两种方式兑底，至少把它抬到最上层。
        let _ = BringWindowToTop(hwnd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        );
        let _ = SetForegroundWindow(hwnd);
    }
}

#[cfg(not(windows))]
pub fn focus_main_window() {}

/// 在资源管理器里定位一个路径：目录就直接打开，文件则打开它所在目录并选中它。
pub fn reveal_path(path: &str) {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        open_path(path);
        return;
    }

    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let op = HSTRING::from("open");
        let file = HSTRING::from("explorer.exe");
        // /select 要把路径整个包在引号里，否则带空格的路径会被拆成两个参数
        let params = HSTRING::from(format!("/select,\"{path}\"").as_str());
        unsafe {
            ShellExecuteW(None, &op, &file, &params, None, SW_SHOWNORMAL);
        }
    }

    #[cfg(not(windows))]
    {
        open_path(path);
    }
}

/// 用系统默认程序打开一个文件/目录（比如用记事本看 dufs 的访问日志）。
pub fn open_path(path: &str) {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let op = HSTRING::from("open");
        let file = HSTRING::from(path);
        unsafe {
            ShellExecuteW(None, &op, &file, None, None, SW_SHOWNORMAL);
        }
    }

    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

/// 把文本放进剪贴板。
///
/// 这里借道 Windows 自带的 clip.exe：内容是纯 ASCII 的 URL，
/// 不存在中文编码问题，且省掉一整套 Win32 剪贴板代码和额外的 feature。
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::process::Stdio;

        let mut cmd = console_command("cmd");
        cmd.args(["/c", "clip"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| crate::i18n::tn(crate::i18n::K::ClipSpawn, &[&e]))?;
        // ★ 无论写得成不成功，都要 `wait` 到底：
        //   1) 不 wait 的话子进程句柄一直吊在那儿（进程句柄泄漏）；
        //   2) `clip` 可能还没读完管道就被我们丢下了，拿不到它的退出码就
        //      无法告诉用户“其实没复制成功”。
        let wrote = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(text.as_bytes()),
            None => Ok(()),
        };
        let status = child.wait();
        wrote.map_err(|e| crate::i18n::tn(crate::i18n::K::ClipWrite, &[&e]))?;
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(crate::i18n::tn(
                crate::i18n::K::ClipExit,
                &[&s.code().unwrap_or(-1)],
            )),
            Err(e) => Err(crate::i18n::tn(crate::i18n::K::ClipWait, &[&e])),
        }
    }

    #[cfg(not(windows))]
    {
        let _ = text;
        Err(crate::i18n::t(crate::i18n::K::ClipUnsupported).to_string())
    }
}
