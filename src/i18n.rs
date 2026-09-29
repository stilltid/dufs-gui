//! Rust 侧的用户可见文案（i18n）。
//!
//! # 为什么要这么写
//!
//! ★ **中文和英文写在同一个键表里**，由 `keys!` 宏同时生成三样东西：
//!   `enum K`（键）、`zh_text()`、`en_text()`。于是：
//!   - **不可能漏翻译** —— 加一个键就必须同时给两种语言，没有第二种写法；
//!   - **不可能漂移** —— 两种语言的键集合是同一份列表生成的，不存在"两套目录对不上"；
//!   - 键是枚举，**拼错编译不过**（这也是当初决定自建、不引 `rust-i18n` 的核心理由）。
//!
//!   代价是翻译要改 Rust 源码，不能只丢一个 json 给译者。
//!   本项目两种语言、一百来条短句，而且我（开发者）就是译者 ——
//!   "编译期就能发现漏翻"比"翻译文件可以单独热改"值钱得多。
//!   如果以后要做 10 门语言、翻译外包给 TMS，就该换成 catalog 文件
//!   （那时把 `keys!` 换成从 json 生成即可，调用点一个字不用改）。
//!
//! # 三条使用规则
//!
//! 1. **用户能看见的串一律走 `t` / `tn` / `tp`**；只在日志里出现的串（`flog!` / `vlog!`）
//!    保持中文原样、**不要**搬进来 —— bug 反馈时日志语言统一更好查（见实施计划 §4.6）。
//! 2. 占位符**只用 `{}`**，不要用 `{:?}` / `{0}` / `{name}`（`tn` 只认 `{}`）。
//! 3. **不要在这里拼句子**：把"谁拼的句子谁翻"落到实处 —— 句子在 Rust 侧拼的，
//!    就在这里提供一个 `pub fn ×××(args) -> String`（见文件末尾那几个函数），
//!    调用点只管传数据。

use std::fmt;

/// 界面语言。
///
/// ★ 它是 Rust 侧文案（`t()` / `tn()`）的语言，与 `config::Language` 是两个不同层次的
///   东西：后者是**用户的选择**（含「跟随系统」），前者是**算完之后的结论**。
///   转换只在一处：`Lang::from_language()`（`service::apply_language` 调它）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    /// 简体中文（**默认**，也是源语言）
    #[default]
    Zh,
    /// English
    En,
}

impl Lang {
    /// 把用户的「选择」换算成「结论」。
    ///
    /// ★ `System` 的判定规则必须和 Slint 框架**逐字一致**（否则界面英文、提示中文）：
    ///   i-slint-core 的 `language_index_from_sys_locale()` 先找语言名完全相等的，
    ///   再比语言主体（`-` / `_` / `@` 之前那段）；我们的语言包只有
    ///   `""`（中文源串）和 `"en"`，所以结果就是「`en*` → 英文，其余 → 中文」。
    ///   这里只比 `en`：德语系统在框架那边会回落中文，这里也必须回中文。
    pub fn from_language(l: crate::config::Language) -> Lang {
        match l {
            crate::config::Language::ZhCn => Lang::Zh,
            crate::config::Language::En => Lang::En,
            crate::config::Language::System => match sys_locale::get_locale() {
                Some(loc) if base_lang(&loc) == "en" => Lang::En,
                _ => Lang::Zh,
            },
        }
    }

    /// 传给 `slint::select_bundled_translation()` 的语言名。
    ///
    /// ★ 中文是 `""` 而不是 `"zh-CN"`：`""` 表示「选编译时那份源串」，
    ///   而源串就是中文（`translations/` 下没有 zh-CN 目录，传 `"zh-CN"` 会报
    ///   LanguageNotFound）。另外 Slint 把 `""` 和 `"en"` 都当成「默认语言」
    ///   处理，但那只在**找不到**名为 `en` 的语言包时才生效 —— 我们有 `en`，
    ///   所以 `"en"` 会真的选到英文（实测过，见 `_tr_probe` 的结论）。
    pub fn slint_tag(self) -> &'static str {
        match self {
            Lang::Zh => "",
            Lang::En => "en",
        }
    }
}

/// 取语言的主体部分（`en-US` / `en_US` → `en`），与 Slint 框架的 `base()` 一致。
fn base_lang(locale: &str) -> &str {
    match locale.find(['-', '_', '@']) {
        Some(i) => &locale[..i],
        None => locale,
    }
}

/// 生成键表。
///
/// 一行一个键：`Variant = "中文" / "English",`
/// 两种语言**必须**同时写 —— 这就是"漏翻编译不过"的实现方式。
macro_rules! keys {
    ($( $(#[$meta:meta])* $variant:ident = $zh:literal / $en:literal , )*) => {
        /// 一个用户可见文案的**键**。
        ///
        /// 命名按**界面位置**（`StatusNotStarted` / `TrayMenuOpen`），
        /// 不按文案含义 —— 这样以后润色中文不用动键。
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum K {
            $( $(#[$meta])* $variant, )*
        }

        impl K {
            /// 全部键。**只有测试用**（遍历检查空翻译与占位符数量），
            /// 所以 `#[cfg(test)]` —— 免得正常构建里报「从未使用」。
            #[cfg(test)]
            pub const ALL: &'static [K] = &[$( K::$variant, )*];
        }

        fn zh_text(k: K) -> &'static str {
            match k { $( K::$variant => $zh, )* }
        }

        fn en_text(k: K) -> &'static str {
            match k { $( K::$variant => $en, )* }
        }
    };
}

keys! {
    // ---------------- 通用 / 程序名 ----------------
    /// 程序名（窗口标题、单实例提示框标题、托盘 tooltip 的前半段）
    AppName = "dufs 文件共享" / "dufs File Sharing",

    // ---------------- 状态栏（底部一行大字） ----------------
    StatusNotStarted = "未启动" / "Not started",
    StatusStarting = "正在启动 dufs…" / "Starting dufs…",
    StatusStartFailed = "启动失败" / "Failed to start",
    StatusStopped = "已停止" / "Stopped",
    /// 服务已启动。`{}` = 端口号
    StatusRunning = "服务已启动 — 监听端口 {}" / "Service running — listening on port {}",

    // ---------------- 关窗提示（底部小灰字，跟着「关闭行为」设置走） ----------------
    HintCloseAsk =
        "关闭窗口时会先弹出确认框（默认动作为直接退出；可在框中改选；勾选「不再询问」后按所选动作执行）。"
        / "Closing the window will ask first (default is to quit; you can change it in the dialog; ticking \"don't ask again\" applies your choice from then on).",
    HintCloseTray =
        "当前设置：关闭窗口后隐藏到右下角托盘，服务继续运行。完全退出请使用托盘右键菜单中的「退出」。"
        / "Current setting: closing the window hides it to the tray and the service keeps running. To fully exit, use \"Quit\" in the tray menu.",
    HintCloseQuit =
        "关闭窗口将停止服务并退出程序。（需要先确认或常驻托盘，请在「应用」页设置。）"
        / "Closing the window stops the service and exits. (To get a confirmation prompt or stay in the tray, change it on the Application page.)",

    // ---------------- 启动期提示 ----------------
    /// 已经有实例在跑（第二个实例弹的消息框）
    AlreadyRunning =
        "dufs 文件共享已在运行。\n\n请在右下角系统托盘中找到其图标。"
        / "dufs File Sharing is already running.\n\nLook for its icon in the system tray.",

    // ---------------- 托盘 ----------------
    TrayMenuOpen = "打开界面" / "Open window",
    TrayMenuStop = "停止服务" / "Stop service",
    TrayMenuCopyUrl = "复制访问地址" / "Copy address",
    TrayMenuQuit = "退出" / "Quit",
    /// 托盘 tooltip：服务运行中。`{}` = 程序名
    TrayTipRunning = "{} — 运行中" / "{} — running",
    /// 托盘 tooltip：服务已停止。`{}` = 程序名
    TrayTipStopped = "{} — 已停止" / "{} — stopped",

    // ---------------- 配置校验（Config::validate）----------------
    // 这些串直接显示在底部提示栏 / 弹框里。
    VldServePathEmpty = "请先选择要共享的文件夹或文件" / "Choose a folder or file to share first",
    /// `{}` = 共享路径
    VldServePathMissing =
        "共享路径不存在，或既不是文件也不是文件夹：{}"
        / "The shared path does not exist, or is neither a file nor a folder: {}",
    VldPortInvalid = "端口必须为 1~65535" / "The port must be between 1 and 65535",
    VldTlsPair =
        "HTTPS 的证书和私钥必须同时指定" / "HTTPS needs both a certificate and a private key",
    /// `{}` = 证书路径
    VldTlsCertMissing =
        "HTTPS 证书文件不存在：{}" / "The HTTPS certificate file does not exist: {}",
    /// `{}` = 私钥路径
    VldTlsKeyMissing =
        "HTTPS 私钥文件不存在：{}" / "The HTTPS private key file does not exist: {}",
    VldAuthNoAccount =
        "已勾选「启用账号访问」，但未填写任何账号。\n\
         此时启动的服务没有任何认证，同一局域网内的任何设备均可访问。\n\
         请至少添加一个账号，或取消勾选。"
        / "Account access is enabled but no account was entered.\n\
           The service would start with no authentication at all, reachable by any device on the LAN.\n\
           Add at least one account, or turn the option off.",
    /// `{}` = 路径
    VldDufsConfigMissing =
        "dufs 配置文件不存在：{}" / "The dufs config file does not exist: {}",
    /// `{}` = 扩展名（不带点）
    VldDufsConfigExt =
        "dufs 配置文件只支持 .json / .yaml / .yml（dufs 0.46 不支持 .{}）"
        / "The dufs config file must be .json / .yaml / .yml (dufs 0.46 does not support .{})",
    /// `{}` = 目录
    VldAssetsMissing =
        "自定义资源目录不存在：{}" / "The custom asset directory does not exist: {}",
    /// `{}` = 目录
    VldAssetsNoIndex =
        "自定义资源目录里缺少 index.html（dufs 要求）：{}"
        / "The custom asset directory has no index.html (dufs requires it): {}",

    // ---------------- 账号列表校验（AccountProblem::describe）----------------
    // 每条都是「第几行 + 出了什么事」，行号由调用点传（对外 1 基）。
    /// `{}` = 行号
    AccAnonymousWithSecret =
        "第 {} 行：匿名访问无需填写密码（用户名留空即为匿名）。"
        / "Row {}: an anonymous account needs no password (leave the username empty for anonymous).",
    /// `{}` = 行号，`{}` = 字符
    AccNameSyntaxChar =
        "第 {} 行：用户名不能包含 `{}`（会破坏 -a 的语法）。"
        / "Row {}: the username cannot contain `{}` (it breaks -a syntax).",
    /// `{}` = 行号
    AccNameSpace = "第 {} 行：用户名不能包含空格。" / "Row {}: the username cannot contain spaces.",
    /// `{}` = 行号
    AccSecretMissing = "第 {} 行：密码不能为空。" / "Row {}: the password cannot be empty.",
    /// `{}` = 行号，`{}` = 字符
    AccSecretSyntaxChar =
        "第 {} 行：密码不能包含 `{}`（会破坏 -a 的语法）。"
        / "Row {}: the password cannot contain `{}` (it breaks -a syntax).",
    /// `{}` = 行号
    AccSecretSpace = "第 {} 行：密码不能包含空格。" / "Row {}: the password cannot contain spaces.",
    /// `{}` = 行号
    AccUnsupportedHash =
        "第 {} 行：密码形似哈希（$...$），但 dufs 仅识别 $6$ 开头的格式。\n\
         其它格式会被当作明文密码本身，等同于未使用哈希。"
        / "Row {}: the password looks like a hash ($...$) but dufs only recognises the $6$ format.\n\
           Anything else is used as a plain-text password, i.e. no hashing at all.",
    /// `{}` = 行号，`{}` = 另一个行号，`{}` = 用户名
    AccConflictingSecret =
        "第 {} 行与第 {} 行是同一用户名（{}），密码必须相同。\n\
         （多行表示该账号的多个路径，密码本身相同。）"
        / "Rows {} and {} use the same username ({}), so the passwords must match.\n\
           (Multiple rows are multiple paths for one account; the password itself is shared.)",
    /// `{}` = 行号
    AccPathMissing =
        "第 {} 行：路径不能为空（至少填写一个 `/`）。"
        / "Row {}: the path cannot be empty (use at least `/`).",
    /// `{}` = 行号，`{}` = 路径
    AccPathNotAbsolute =
        "第 {} 行：路径必须以 / 开头：“{}”。"
        / "Row {}: the path must start with /: '{}'.",
    /// `{}` = 行号
    AccPathSpace = "第 {} 行：路径不能包含空格。" / "Row {}: the path cannot contain spaces.",
    /// `{}` = 行号，`{}` = 字符
    AccPathSyntaxChar =
        "第 {} 行：路径不能包含 `{}`（会破坏 -a 的语法，规则会失效）。"
        / "Row {}: the path cannot contain `{}` (it breaks -a syntax and the rule stops working).",

    // ---------------- 启动 dufs（dufs.rs）----------------
    /// `{}` = 端口
    StartPortReserved =
        "端口 {} 位于 Windows 的保留端口段内，任何程序都无法绑定。\n\
         安装过 Hyper-V / WSL / Docker 的机器常见此问题。请更换端口（例如 {}）。"
        / "Port {} is inside Windows' reserved port range, so no program can bind it.\n\
           This is common on machines with Hyper-V / WSL / Docker installed. Please pick another port (e.g. {}).",
    /// `{}` = 端口
    StartPortInUse = "端口 {} 已被其它程序占用，请更换端口。" / "Port {} is already in use by another program. Please pick another port.",

    /// `{}` = 额外参数的选项名
    OptCliOnly =
        "「额外命令行参数」中不能填写 `--{}`：该选项仅输出信息后退出，服务将无法启动。\n请删除该项。"
        / "The extra command-line arguments cannot contain `--{}`: that option only prints information and exits, so the service cannot start.\nPlease remove it.",
    /// `{}` = 额外参数的选项名
    OptDuplicate =
        "「额外命令行参数」中的 `--{}` 与界面上的控件重复。\n\
         dufs 不允许同一选项出现多次，会直接报\n\
         「error: the argument '--{}' cannot be used multiple times」然后退出。\n\
         界面已有该选项对应的开关，请二者择一（推荐使用界面的开关）。"
        / "The extra command-line arguments contain `--{}`, which is already controlled by a setting in the UI.\n\
           dufs rejects an option that appears more than once and exits with\n\
           \"error: the argument '--{}' cannot be used multiple times\".\n\
           Please keep only one of them (the UI switch is recommended).",

    /// `{}` = 系统错误
    StartSpawnFailed = "启动 dufs 失败：{}" / "Failed to start dufs: {}",
    /// `{}` = 系统错误
    StartJobFailed = "创建 Job Object 失败：{}" / "Failed to create the Job Object: {}",

    /// `{}` = 退出状态，`{}` = 子进程输出
    StartExitedImmediately =
        "dufs 启动后立即退出（{}）。\n{}" / "dufs exited immediately after starting ({}).\n{}",
    /// 子进程没有任何输出时的占位
    StartNoOutput = "（没有捕获到输出）" / "(no output captured)",
    /// `{}` = 系统错误
    StartProbeFailed = "查询子进程状态失败：{}" / "Failed to check the child process state: {}",
    /// `{}` = 秒数，`{}` = 地址，`{}` = 子进程输出
    StartWaitTimeout =
        "等待 dufs 就绪超时（{} 秒）。无法连接 {}，可能被防火墙拦截或端口被其它程序占用。\n{}"
        / "Timed out waiting for dufs to become ready ({} s). Could not connect to {}; the firewall may be blocking it, or the port is taken by another program.\n{}",

    /// `{}` = 目录
    ExtractDirFailed = "创建目录 {} 失败：{}" / "Failed to create the directory {}: {}",
    /// `{}` = 路径
    ExtractWriteFailed = "写入 {} 失败：{}" / "Failed to write {}: {}",
    /// `{}` = 系统错误
    ExtractDufsFailed = "释放 dufs 失败：{}" / "Failed to extract dufs: {}",
    /// `{}` = 文件名，`{}` = 系统错误
    ExtractAssetFailed = "释放 {} 失败：{}" / "Failed to extract {}: {}",

    // ---------------- 防火墙（firewall.rs）----------------
    // ---------------- 主界面提示 / 确认框（service.rs、callbacks.rs）----------------
    VldPortEmpty = "请填写端口号（1 ~ 65535）" / "Please enter a port number (1 ~ 65535)",
    /// `{}` = 用户输入的原文（带引号）
    VldPortNotInt =
        "端口必须为 1 ~ 65535 的整数，当前为 {}"
        / "The port must be an integer between 1 and 65535; it is currently {}",
    VldPortZero = "端口必须为 1 ~ 65535" / "The port must be between 1 and 65535",

    TopmostWindowNotFound = "找不到主窗口，置顶切换失败" / "Could not find the main window; toggling always-on-top failed",
    /// `{}` = 系统错误
    TopmostOs = "窗口置顶切换失败（{}）" / "Toggling always-on-top failed ({})",
    TopmostUnsupported = "当前平台不支持窗口置顶" / "Always-on-top is not supported on this platform",

    /// 用户手改配置指定了一个不在候选里的监听地址时，列表里给它显示的名字
    IfaceSpecified = "指定的监听地址" / "Specified address",
    IfaceAllBracket = "「所有网卡」" / "\"All interfaces\"",
    /// `{}` = 逗号/顿号连起来的地址列表
    IfaceBoundSummary = "已绑定：{}" / "Bound to: {}",
    /// `{}` = 已经不存在的地址列表
    IfaceStaleRemoved =
        "原选择的监听地址 {} 已不存在（更换网络、拔出网线或禁用网卡均会导致此情况），已自动移除。"
        / "The previously selected address {} no longer exists (changing networks, unplugging the cable or disabling a network adapter all cause this); it has been removed automatically.",
    /// `{}` = 已经不存在的地址列表，`{}` = 现在用的（`「所有网卡」` 或 `「192.168.1.5」`）
    IfaceStaleSwitched =
        "原选择的监听地址 {} 在本机已不存在（更换网络、拔出网线或禁用网卡均会导致此情况），已改用 {}。\n"
        / "The previously selected address {} no longer exists on this machine (changing networks, unplugging the cable or disabling a network adapter all cause this). Switched to {}.\n",

    UrlNoLanIp = "未检测到可用的局域网 IP" / "No usable LAN IP address was detected",
    /// `{}` = 网卡名
    UrlVirtualIface = "虚拟网卡（{}），手机可能无法连接" / "Virtual adapter ({}); your phone may not be able to connect",

    /// `{}` = 「、下载这个文件」这类动宾短语（下面三条之一）
    HintNoAuth =
        "未启用访问认证：同一局域网内的任何设备均可访问{}\n停止服务后可在「共享 → 账号」中设置用户名与密码。"
        / "Access authentication is off: any device on the same LAN can access{}\nYou can set a user name and password in Share → Accounts after stopping the service.",
    HintNoAuthVerbFileOnly =
        "、下载该文件（单文件模式下仅能读取，无法上传）"
        / " and download this file (single-file mode is read-only, so uploads are not possible)",
    HintNoAuthVerbDownload = "、下载这些文件" / " and download these files",
    HintNoAuthVerbUpload = "并上传文件" / " and upload files",
    HintFirewallTip =
        "提示：若手机扫码后无法打开，请点「放行防火墙」允许 Windows 放行该端口。"
        / "Tip: if the page does not open after scanning the QR code, click \"Allow through firewall\" to let Windows allow this port.",

    FwRulesNone = "尚未为 dufs 添加防火墙放行规则。" / "No firewall rule has been added for dufs yet.",
    /// 复数：只有 1 条规则
    FwRulesOne = "已放行端口：{}（共 1 条规则）" / "Allowed port: {} (1 rule)",
    /// 复数：`{}` = 端口列表，`{}` = 规则条数
    FwRulesMany = "已放行端口：{}（共 {} 条规则）" / "Allowed ports: {} ({} rules)",
    FwRulesManyExtra =
        "\n存在多条规则，说明此前更换过端口，旧规则可一并清理。"
        / "\nMore than one rule exists, which means the port was changed before; the old rules can be cleaned up together.",
    FwCleaningRequest = "正在请求管理员权限以清理防火墙规则…" / "Requesting administrator permission to clean up firewall rules…",
    /// `{}` = 错误
    FwCleanFailed = "清理防火墙规则失败：{}" / "Cleaning up firewall rules failed: {}",

    SysNoDataDir = "无法定位程序数据目录" / "Could not locate the program data directory",

    PurgeRunningWarn = "服务正在运行，将先自动停止。\n\n" / "The service is running and will be stopped first.\n\n",
    PurgeConfirmTitle = "清除程序数据" / "Erase program data",
    /// `{}` = 数据目录，`{}` = config.json 路径
    PurgeConfirmText =
        "是否清除 dufs-gui 在本机留下的全部数据？\n\n\
         将删除：\n  · {}\n  · {}\n\n\
         不会删除：\n  · 共享文件夹\n  · dufs-gui.exe 本体（卸载时删除该文件即可）\n\n\
         重新打开程序后将恢复默认设置。"
        / "Erase all data that dufs-gui has left on this machine?\n\n\
           Will be deleted:\n  · {}\n  · {}\n\n\
           Will NOT be deleted:\n  · your shared folders\n  · dufs-gui.exe itself (just delete that file to uninstall)\n\n\
           Default settings are restored the next time the program starts.",
    FwPurgeConfirmTitle = "防火墙规则" / "Firewall rules",
    FwPurgeConfirmText =
        "是否同时清理 Windows 防火墙中为 dufs 添加的放行规则？\n\n\
         规则名形如 dufs-gui-5000，按放行过的端口命名（更换端口后会存在多条）。\n\
         清理需要管理员权限，会弹出一次 UAC。\n\n\
         选择「否」则保留。该操作仅影响 Windows 防火墙，不影响共享文件夹。"
        / "Also remove the inbound rules that were added to Windows Firewall for dufs?\n\n\
           Rules are named after the allowed port, e.g. dufs-gui-5000 (changing the port leaves more than one behind).\n\
           Cleanup needs administrator permission and raises one UAC prompt.\n\n\
           Choose \"No\" to keep them. This only affects Windows Firewall, not your shared folders.",
    /// `{}` = 已删除的文件列表，`{}` = 防火墙规则的处置
    PurgeDone =
        "已清除：\n{}\n\n后续操作：\n  · 如不再需要本程序，直接删除 dufs-gui.exe 即可\n  · 防火墙规则{}"
        / "Erased:\n{}\n\nNext steps:\n  · if you no longer need the program, just delete dufs-gui.exe\n  · firewall rules: {}",
    PurgeNoFiles = "（未找到需要删除的文件）" / "(no files needed deleting)",
    FwPurged = "已同时请求清理" / "cleanup requested",
    FwKept = "已按要求保留" / "kept as requested",
    /// `{}` = 删除失败的列表，`{}` = 已删除的列表
    PurgePartialFail =
        "部分内容删除失败（可能正被其它程序占用，关闭后重试）：\n{}\n\n已删除：\n{}"
        / "Some items could not be deleted (another program may be using them; close it and try again):\n{}\n\nDeleted:\n{}",

    LogNotConfigured =
        "尚未设置访问日志文件。请在「进阶」页填写文件路径（例如 C:\\Users\\<用户名>\\dufs-access.log），重启服务后可查看。"
        / "No access log file is set. Enter a path on the Advanced page (for example C:\\Users\\<user>\\dufs-access.log) and restart the service to view it.",
    /// `{}` = 路径
    LogMissing = "日志文件尚不存在：{}" / "The log file does not exist yet: {}",
    LogAppEmpty = "暂无程序日志（本次启动后未记录任何内容）" / "No program log yet (nothing has been recorded since this launch)",
    LogAppCleared = "程序日志已清空。" / "The program log has been cleared.",
    /// `{}` = 路径
    FileOpenedInExplorer = "已在资源管理器里打开：{}" / "Opened in File Explorer: {}",
    CfgFileMissing = "暂无配置文件（修改任意设置后会自动生成）" / "No config file yet (one is created as soon as you change any setting)",

    RestoreAppTitle = "恢复本程序默认设置" / "Restore this program's defaults",
    RestoreAppDesc =
        "将开启允许上传与允许搜索（便于手机向电脑传输文件）。"
        / "Uploads and search will be enabled (handy for sending files from your phone to the PC).",
    RestoreDufsTitle = "按 dufs 原生默认恢复" / "Restore dufs's own defaults",
    RestoreDufsDesc =
        "全部权限将被关闭，服务变为纯只读，访问方仅能查看与下载。"
        / "All permissions are turned off: the service becomes read-only and visitors can only browse and download.",
    /// `{}` = 选项说明（上面两条之一）
    RestoreConfirmText =
        "{}\n\n\
         同时恢复默认的还有：\n\
         · 文件操作权限、打包与校验、符号链接、放开全部操作、隐藏内容、CORS\n\
         · 用户名密码与认证规则、URL 前缀\n\
         · 静态网站模式、HTTPS、访问日志、dufs 配置文件、前端资源、额外参数\n\
         · 端口恢复为 5000、监听网卡恢复为「所有网卡」\n\n\
         不会改动：共享路径、窗口置顶、关闭行为、详细日志。\n\n\
         该操作无法撤销。"
        / "{}\n\n\
           The following are also restored to their defaults:\n\
           · file operation permissions, archiving and checksums, symlinks, allowing all operations, hidden files, CORS\n\
           · user name and password, auth rules, URL prefix\n\
           · static website mode, HTTPS, access log, dufs config file, frontend assets, extra arguments\n\
           · the port goes back to 5000 and the listening interface back to \"All interfaces\"\n\n\
           Not changed: shared path, always-on-top, close behaviour, verbose log.\n\n\
           This cannot be undone.",
    /// `{}` = 恢复成了哪套默认（`恢复本程序默认设置` / `按 dufs 原生默认恢复`）
    RestoreDone =
        "已恢复默认设置：{}。端口与监听网卡也已恢复默认。"
        / "Defaults restored: {}. The port and the listening interface were reset too.",

    // ---------------- 文件选择框 / 界面提示（callbacks.rs、config.rs）----------------
    PickTlsCertTitle = "选择 TLS 证书文件" / "Choose the TLS certificate file",
    PickTlsCertFilter = "证书" / "Certificate",
    PickTlsKeyTitle = "选择 TLS 私钥文件" / "Choose the TLS private key file",
    PickTlsKeyFilter = "私钥" / "Private key",
    PickDufsConfigTitle = "选择 dufs 配置文件" / "Choose the dufs config file",
    PickDufsConfigFilter = "dufs 配置" / "dufs config",
    PickAssetsTitle =
        "选择自定义前端资源目录（需含 index.html）"
        / "Choose a custom frontend assets folder (must contain index.html)",
    PickServeDirTitle = "选择要共享的文件夹" / "Choose the folder to share",
    PickServeFileTitle = "选择要共享的文件" / "Choose the file to share",

    HintRequestingAdmin = "正在请求管理员权限…" / "Requesting administrator permission…",
    /// `{}` = 端口
    HintFwRequested =
        "已请求放行 TCP 端口 {}。若出现 UAC 提示，请选择「是」。"
        / "Asked to allow inbound TCP port {}. If a UAC prompt appears, choose \"Yes\".",
    HintUrlCopied = "地址已复制到剪贴板" / "The address was copied to the clipboard",
    HintServePathEmpty = "尚未选择共享路径" / "No shared path has been chosen yet",
    /// `{}` = 路径
    HintServePathMissing = "共享路径不存在：{}" / "The shared path does not exist: {}",
    HintNoUrlYet = "暂无访问地址（服务未启动）" / "No access address yet (the service is not running)",
    HintNoAccount =
        "没有可用的账号密码（请在「共享 → 账号」中填写一行用户名与密码）"
        / "No usable user name and password (fill in one row under Share → Accounts)",
    /// `{}` = 用户名
    HintAuthUrlCopied =
        "已复制账号 {} 的访问链接。该链接包含明文密码，请勿外传。"
        / "Copied the access link for {}. The link contains the password in plain text, so keep it to yourself.",
    /// `{}` = 路径
    HintPathGone = "该路径已不存在：{}" / "That path no longer exists: {}",

    AutostartServeTitle = "自动开始共享" / "Start sharing automatically",
    AutostartServeWarn =
        "尚未启用账号认证（「共享 → 账号」）。\n\n\
         自动开始共享后，同一局域网内任何知道地址的设备均可访问文件，\n\
         可执行的操作取决于「允许的操作」中的勾选。\n\n\
         仍要开启自动共享吗？"
        / "Account authentication is not enabled (Share → Accounts).\n\n\
           Once automatic sharing starts, any device on the same LAN that knows the address can access your files,\n\
           and what it can do depends on the boxes ticked under \"Allowed operations\".\n\n\
           Start sharing automatically anyway?",
    AutostartServeWarnBoot =
        "本程序已设置「启动后自动开始共享」，但尚未启用账号认证。\n\n\
         同一局域网内任何知道地址的设备均可访问文件。\n\n\
         仍要自动开始共享吗？"
        / "This program is set to start sharing automatically when it opens, but account authentication is not enabled.\n\n\
           Any device on the same LAN that knows the address can access your files.\n\n\
           Start sharing automatically anyway?",
    AutostartResultTrayServe =
        "已设置为登录后静默启动到托盘，并自动开始共享。"
        / "Set to start silently in the tray at logon and to begin sharing automatically.",
    AutostartResultServe =
        "已设置为登录后自动运行，并自动开始共享。"
        / "Set to run at logon and to begin sharing automatically.",
    AutostartResultTrayOnly =
        "已设置为登录后静默启动到托盘（不自动开始共享）。"
        / "Set to start silently in the tray at logon (without starting to share automatically).",
    AutostartResultRunOnly =
        "已设置为登录后自动运行（不自动开始共享）。"
        / "Set to run at logon (without starting to share automatically).",
    AutostartResultServeOnly =
        "已设置为每次打开本程序时自动开始共享。"
        / "Set to begin sharing automatically every time this program opens.",
    AutostartResultOff = "已取消开机自启与自动共享。" / "Autostart at logon and automatic sharing are now off.",

    DropUnrecognized = "未识别到可共享的文件或文件夹。" / "No shareable file or folder was recognised.",
    /// `{}` = 一次拖进来的个数
    DropTooMany =
        "一次仅支持拖入一个文件或文件夹，本次拖入 {} 个，已全部忽略。请一次只拖入一个。"
        / "Only one file or folder can be dropped at a time; {} were dropped and all of them were ignored. Please drop one at a time.",
    /// `{}` = 路径
    DropBadPath =
        "该路径无法共享（不存在，或既不是文件也不是文件夹）：{}"
        / "That path cannot be shared (it does not exist, or it is neither a file nor a folder): {}",

    // ---------------- 开机自启 / 注册表（autostart.rs）----------------
    /// `{}` = 动作名（下面两条之一），`{}` = 注册表错误码
    RegOpFailed = "{}失败（注册表错误码 {}）" / "Failed to {} (registry error code {})",
    RegOpWrite = "写入开机自启项" / "write the autostart entry",
    RegOpDelete = "删除开机自启项" / "delete the autostart entry",
    /// `{}` = 系统错误
    AutoNoExePath = "获取当前程序路径失败：{}" / "Could not get the path of the current program: {}",
    AutoEmptyExePath = "当前程序路径为空" / "The program path is empty",
    AutoRunKeyLocked =
        "无法打开注册表的 Run 项（可能受策略限制）"
        / "Could not open the registry Run key (it may be restricted by policy)",
    /// ★ 只在非 Windows 构建里会被构造，见 `FirewallError::Unsupported`
    #[cfg_attr(windows, allow(dead_code))]
    AutoUnsupported = "当前平台不支持开机自启" / "Autostart at logon is not supported on this platform",

    /// 列表分隔符。中文用顿号，英文用逗号加空格
    ListSeparator = "、" / ", ",
    /// 空列表的占位
    ListEmpty = "（无）" / "(none)",

    FwUnauthorized =
        "未获得管理员授权（UAC 被取消或拒绝）。\n\
         若手机无法连接，请再次点击「放行防火墙」并在弹窗中选择「是」。"
        / "Administrator permission was not granted (the UAC prompt was cancelled or refused).\n\
           If your phone still cannot connect, click \"Allow through firewall\" again and choose \"Yes\" in the prompt.",
    /// `{}` = 系统错误
    FwNoSelfPath = "取不到自身路径: {}" / "Cannot determine the program path: {}",
    FwUnsupported = "非 Windows 平台无需配置防火墙" / "Firewall configuration is not needed on non-Windows platforms",
    FwUacCancelled = "操作已取消（UAC 提权未通过）" / "Cancelled (the UAC prompt was not accepted)",
    /// `{}` = ShellExecuteW 的返回值
    FwElevateFailed =
        "无法启动提权实例（ShellExecuteW 返回 {}）"
        / "Could not launch the elevated instance (ShellExecuteW returned {})",

    /// 清理规则时的弹窗标题
    FwCleanCaption = "dufs 防火墙清理" / "dufs firewall cleanup",
    FwCleanNothing =
        "未找到 dufs-gui 的防火墙放行规则，无需清理。"
        / "No dufs-gui firewall rules were found, nothing to clean up.",
    /// `{}` = 条数，`{}` = 规则名列表
    FwCleaned =
        "已删除 {} 条放行规则：\n{}" / "Removed {} firewall rules:\n{}",
    /// `{}` = 已删除列表，`{}` = 失败列表
    FwCleanPartial =
        "部分规则删除失败。\n\n已删除：{}\n失败：{}"
        / "Some rules could not be removed.\n\nRemoved: {}\nFailed: {}",

    /// `{}` = 端口，`{}` = 规则名
    FwInstallOk =
        "已为 dufs 放行入站 TCP 端口 {}。\n\n\
         生效范围：专用网络 / 域网络（公共网络未放行）\n\
         规则名称：{}\n\n\
         可返回 dufs 界面重新启动服务，然后用手机扫码访问。"
        / "Inbound TCP port {} has been allowed for dufs.\n\n\
           Applies to: private / domain networks (public networks are not allowed)\n\
           Rule name: {}\n\n\
           Go back to the dufs window, start the service again, then scan the QR code with your phone.",
    FwInstallCaptionOk = "dufs 防火墙设置成功" / "dufs firewall configured",
    FwInstallCaptionFail = "dufs 防火墙设置失败" / "dufs firewall setup failed",
    /// `{}` = netsh 的错误输出，`{}` = 端口
    FwInstallFail =
        "添加防火墙规则失败：\n{}\n\n\
         可手动放行：\n\
         控制面板 → Windows Defender 防火墙 → 高级设置 → 入站规则 → 新建规则\n\
         类型选择「端口」，协议 TCP，端口 {}，动作选择「允许连接」。"
        / "Failed to add the firewall rule:\n{}\n\n\
           You can allow it manually:\n\
           Control Panel → Windows Defender Firewall → Advanced settings → Inbound Rules → New Rule\n\
           Choose \"Port\", protocol TCP, port {}, action \"Allow the connection\".",

    /// `{}` = 命令名，`{}` = 系统错误
    FwNetshSpawn = "无法执行 {}: {}" / "Could not run {}: {}",
    /// `{}` = 退出码，`{}` = stdout，`{}` = stderr
    FwNetshExit = "netsh 退出码 {}\n{}{}" / "netsh exited with code {}\n{}{}",

    /// `{}` = 系统错误
    ClipSpawn = "无法启动剪贴板程序：{}" / "Could not start the clipboard helper: {}",
    /// `{}` = 系统错误
    ClipWrite = "写入剪贴板失败：{}" / "Failed to write to the clipboard: {}",
    /// `{}` = 退出码
    ClipExit = "剪贴板程序退出码 {}" / "The clipboard helper exited with code {}",
    /// `{}` = 系统错误
    ClipWait = "等待剪贴板程序失败：{}" / "Failed while waiting for the clipboard helper: {}",
    /// ★ 只在非 Windows 构建里会被构造（那边的分支是 cfg 掉的），见 `FirewallError::Unsupported`
    #[cfg_attr(windows, allow(dead_code))]
    ClipUnsupported = "当前平台不支持剪贴板复制" / "Clipboard copy is not supported on this platform",
}

// ============================================================
// 运行期：当前语言 + 查表 / 填占位符
// ============================================================

use std::sync::atomic::{AtomicU8, Ordering};

/// 当前语言。★ 启动时定一次，之后不再变 —— 实施计划 §6-4 定的是
/// 「改语言后重启生效」，所以这里不需要任何通知机制。
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 当前界面语言
pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Zh,
    }
}

/// 切换界面语言。**必须在任何界面文案被取用之前调**。
///
/// ★ 它只管 Rust 侧；Slint 侧要另外调 `select_bundled_translation()` ——
///   两件事都包在 `service::apply_language()` 里，别单独调这一个。
#[allow(dead_code)]
pub fn set_lang(l: Lang) {
    CURRENT.store(
        match l {
            Lang::Zh => 0,
            Lang::En => 1,
        },
        Ordering::Relaxed,
    );
}

/// 取一条无参数文案
pub fn t(k: K) -> &'static str {
    match lang() {
        Lang::Zh => zh_text(k),
        Lang::En => en_text(k),
    }
}

/// 取一条带占位符的文案，并把 `{}` **按顺序**替换成 `args`。
///
/// ★ 参数给少了**保留 `{}` 原样，不 panic** —— 这是界面文案，
///   代价应该是"看得见的括号"，不是整个程序挂掉。
pub fn tn(k: K, args: &[&dyn fmt::Display]) -> String {
    fill(t(k), args)
}

// ★ 复数没有做成通用的 `tp(one, other, n)`：那样只能填一个占位符，
//   而真实的复数句子往往还带别的参数（比如「已放行端口：{}（共 {} 条规则）」）。
//   所以复数一律走下面的**逐句助手函数** —— 每个句子自己知道有几个占位符，
//   规则也就一条：`n == 1` 用单数形式（中/英两种语言的规则，
//   见实施计划 §6-3「自写极简助手，不引 CLDR」）。

// ============================================================
// 「谁拼句子谁翻」的落地：句子在 Rust 侧拼的，就提供函数
// ============================================================

/// 复数句选择器：`n == 1` 取 `one`，否则取 `many`。
///
/// ★ 为什么不是一个通用的 `tp(one, other, n)`：真实句子除了数量往往还带别的参数
/// （「已放行端口：{}（共 {} 条规则）」的第一个 `{}` 是端口列表），
/// 通用函数只能填一个占位符，所以改成"哪个句子自己知道要几个参数"。
/// 中文没有单复数，两个键给一样的句子即可（数词照样写对：`共 1 条规则`）。
pub fn pl(
    one: K,
    many: K,
    n: usize,
    args_one: &[&dyn fmt::Display],
    args_many: &[&dyn fmt::Display],
) -> String {
    if n == 1 { tn(one, args_one) } else { tn(many, args_many) }
}

/// 列表分隔符（中文用顿号、英文用逗号加空格）—— 拼列表时用它，别写死 "、"。
pub fn list_sep() -> &'static str {
    t(K::ListSeparator)
}

/// 托盘 tooltip（`{} 运行中` / `{} 已停止`）
pub fn tray_tooltip(running: bool) -> String {
    let k = if running {
        K::TrayTipRunning
    } else {
        K::TrayTipStopped
    };
    tn(k, &[&t(K::AppName)])
}

/// 把模板里的 `{}` 按顺序替换掉
fn fill(template: &str, args: &[&dyn fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut args = args.iter();
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        match args.next() {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    //! 键表由宏生成，"两种语言都在"是**结构性保证**；这里钉的是宏管不了的两件事。

    use super::*;

    /// 空翻译 + 占位符数量不一致。
    ///
    /// ★ 后者是真会造成线上问题的：英文漏写一个 `{}`，那句英文就会把
    ///   `{{}}` 之外的参数丢掉或把括号显示给用户，而编译器不会吭声。
    #[test]
    fn 没有空翻译且占位符数量一致() {
        for k in K::ALL {
            let (zh, en) = (zh_text(*k), en_text(*k));
            assert!(!zh.trim().is_empty(), "{k:?} 的中文是空的");
            assert!(!en.trim().is_empty(), "{k:?} 的英文是空的");
            assert_eq!(
                zh.matches("{}").count(),
                en.matches("{}").count(),
                "{k:?} 两种语言的占位符数量不一致"
            );
        }
    }

    /// 切换语言真的换了文案。
    ///
    /// ★ 这是**唯一**碰全局语言状态的测试，所以要用锁串行化 ——
    ///   其余测试一律直接调 `zh_text` / `en_text`，不依赖全局。
    #[test]
    fn 切换语言生效() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        set_lang(Lang::Zh);
        assert_eq!(t(K::StatusNotStarted), "未启动");
        set_lang(Lang::En);
        assert_eq!(t(K::StatusNotStarted), "Not started");
        set_lang(Lang::Zh);
        assert_eq!(t(K::StatusNotStarted), "未启动");
    }

    #[test]
    fn 占位符按顺序替换() {
        // 直接对两种语言的原文做替换，不碰全局语言状态（免得和其它测试抢）
        assert_eq!(
            fill(zh_text(K::StatusRunning), &[&5000]),
            "服务已启动 — 监听端口 5000"
        );
        assert_eq!(
            fill(en_text(K::StatusRunning), &[&5000]),
            "Service running — listening on port 5000"
        );
        // 参数给少了：保留 {} 而不是 panic
        assert_eq!(fill(zh_text(K::StatusRunning), &[]), "服务已启动 — 监听端口 {}");
        // 参数给多了：多的丢掉，不报错
        assert_eq!(fill(zh_text(K::AppName), &[&1, &2]), "dufs 文件共享");
    }

    // ============================================================
    // Slint 侧的漏翻检查（`.slint` 的 @tr 串 ⇄ translations/en/....po）
    // ============================================================
    //
    // ★ 为什么必须有这条测试：Rust 侧的漏翻**编译不过**（键表两种语言写在一行），
    //   但 Slint 侧的漏翻是**静默的** —— `@tr("中文")` 在 .po 里找不到条目时，
    //   英文界面会直接把中文显示出来，编译器一声不吭。
    //   所以这条测试是那个"编译期保证"在 Slint 侧的唯一替代品。
    //
    // ★ 它同时查两个方向：
    //   ① 源码里有、.po 里没有 → 漏翻（上面那个静默失败）；
    //   ② .po 里有、源码里没有 → 没人用的死条目（改文案时改了源码忘了 .po，
    //      或者写错了 msgid）。放着不管会让"这条到底还有没有用"变得查不清。
    mod slint_catalog {
        use std::path::{Path, PathBuf};

        /// 语言包路径（相对项目根目录）。目录关系是 Slint 定的：
        /// `translations/<语言>/LC_MESSAGES/<CARGO_PKG_NAME>.po`
        const PO: &str = "translations/en/LC_MESSAGES/dufs-gui.po";

        fn root() -> PathBuf {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        }

        /// 递归收集 `ui/` 下所有 `.slint`
        fn slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).expect("读 ui 目录失败").flatten() {
                let p = e.path();
                if p.is_dir() {
                    slint_files(&p, out);
                } else if p.extension().is_some_and(|x| x == "slint") {
                    out.push(p);
                }
            }
        }

        /// 把 Slint 字符串字面量里的转义还原成**运行时字符串**。
        ///
        /// ★ 这一步是整条链里最容易搞错的地方：`.po` 里的 msgid 必须等于
        ///   运行时那个串。源码里写 `\n`（两字符）而运行时是换行（一字符），
        ///   `.po` 里写 `\n` 由 gettext 解析成换行 —— 所以两边都要还原。
        fn unescape(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            let mut it = s.chars();
            while let Some(c) = it.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match it.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some(o) => out.push(o),
                    None => {}
                }
            }
            out
        }

        /// 从 `.slint` 里取出所有 `@tr("...")` 的源串
        fn source_msgids() -> Vec<String> {
            let mut files = Vec::new();
            slint_files(&root().join("ui"), &mut files);
            assert!(!files.is_empty(), "ui/ 下没找到 .slint 文件？");
            let mut out = Vec::new();
            for f in files {
                let text = std::fs::read_to_string(&f).expect("读 .slint 失败");
                let bytes: Vec<char> = text.chars().collect();
                let mut i = 0;
                while i < bytes.len() {
                    // 跳过行注释：文档注释里会引示例代码（tip.slint 里就有一个
                    // `@tr("这句说明")` 的例子），不跳的话测试会去 .po 里找它。
                    // ★ 只在行首或空白后认 `//`，免得把字符串里的 "https://…" 当成注释。
                    if bytes[i] == '/'
                        && bytes.get(i + 1) == Some(&'/')
                        && (i == 0 || bytes[i - 1].is_whitespace())
                    {
                        while i < bytes.len() && bytes[i] != '\n' {
                            i += 1;
                        }
                        continue;
                    }
                    // 找 `@tr("`
                    if bytes[i] == '@' && bytes.get(i + 1) == Some(&'t') && bytes.get(i + 2) == Some(&'r')
                    {
                        // 跳过 `@tr` 和空白，要求紧跟 `(`
                        let mut j = i + 3;
                        while bytes.get(j).is_some_and(|c| c.is_whitespace()) {
                            j += 1;
                        }
                        if bytes.get(j) == Some(&'(') {
                            j += 1;
                            while bytes.get(j).is_some_and(|c| c.is_whitespace()) {
                                j += 1;
                            }
                            if bytes.get(j) == Some(&'"') {
                                j += 1;
                                let start = j;
                                while let Some(&c) = bytes.get(j) {
                                    if c == '\\' {
                                        j += 2;
                                        continue;
                                    }
                                    if c == '"' {
                                        break;
                                    }
                                    j += 1;
                                }
                                let raw: String = bytes[start..j.min(bytes.len())].iter().collect();
                                out.push(unescape(&raw));
                                i = j;
                                continue;
                            }
                        }
                    }
                    i += 1;
                }
            }
            out
        }

        /// 极简 `.po` 解析：只要 msgid / msgstr 与它们的续行，够本测试用。
        ///
        /// ★ 刻意不引 `rspolib`：那会为了一个开发期检查把 gettext 解析器
        ///   拉进依赖树（构建期依赖也是依赖，本项目连运行库都不想带）。
        fn po_entries() -> Vec<(String, String)> {
            let text = std::fs::read_to_string(root().join(PO)).expect("读 .po 失败");
            let mut out: Vec<(String, String)> = Vec::new();
            let mut id: Option<String> = None;
            let mut str_: Option<String> = None;
            let flush = |id: &mut Option<String>, str_: &mut Option<String>, out: &mut Vec<_>| {
                if let Some(i) = id.take() {
                    out.push((i, str_.take().unwrap_or_default()));
                }
            };
            for line in text.lines() {
                let l = line.trim();
                if let Some(rest) = l.strip_prefix("msgid ") {
                    flush(&mut id, &mut str_, &mut out);
                    id = Some(unescape(rest.trim_matches('"')));
                } else if let Some(rest) = l.strip_prefix("msgstr ") {
                    str_ = Some(unescape(rest.trim_matches('"')));
                } else if l.starts_with('"') {
                    let piece = unescape(l.trim_matches('"'));
                    match (&mut str_, &mut id) {
                        // 续行：msgstr 已经开始 → 接在译文后面
                        (Some(s), _) => s.push_str(&piece),
                        // 还没出现 msgstr → 接在 msgid 后面
                        (None, Some(i)) => i.push_str(&piece),
                        (None, None) => {}
                    }
                }
            }
            flush(&mut id, &mut str_, &mut out);
            // 第一条是头部（msgid ""），不算条目
            out.retain(|(i, _)| !i.is_empty());
            out
        }

        /// ★ 核心断言：两个集合必须**完全相等**。
        #[test]
        fn slint的翻译不缺失也不多余() {
            let mut src: Vec<String> = source_msgids();
            src.sort();
            src.dedup();
            let po: Vec<(String, String)> = po_entries();
            let mut po_ids: Vec<String> = po.iter().map(|(i, _)| i.clone()).collect();
            po_ids.sort();
            po_ids.dedup();

            let missing: Vec<&String> = src.iter().filter(|s| !po_ids.contains(s)).collect();
            let extra: Vec<&String> = po_ids.iter().filter(|s| !src.contains(s)).collect();
            assert!(
                missing.is_empty(),
                "这些串在 .slint 里用了 @tr，但 {PO} 里没有译文（英文界面会显示中文）：\n{missing:#?}"
            );
            assert!(
                extra.is_empty(),
                "{PO} 里有源码中已不存在的条目（改了文案没同步，或 msgid 写错了）：\n{extra:#?}"
            );
            assert_eq!(src.len(), po_ids.len());
            // ★ 防"空洞测试"：万一两边的解析器都挂了（都返回空集合），
            //   上面三条断言会全绿却什么都没查。数量在这里钉下限。
            assert!(src.len() > 100, "只解析到 {} 条 @tr，解析器多半坏了", src.len());
        }

        /// 空译文同样是"漏翻"：`msgstr ""` 在 gettext 里表示"未翻译"。
        #[test]
        fn slint的译文都不为空() {
            let empty: Vec<String> = po_entries()
                .into_iter()
                .filter(|(_, s)| s.trim().is_empty())
                .map(|(i, _)| i)
                .collect();
            assert!(empty.is_empty(), "这些条目没有译文：\n{empty:#?}");
        }

        /// 占位符与换行必须和源串对得上。
        ///
        /// ★ 换行对不上是最隐蔽的一类错：`.po` 里少一个 `\n`，那段长提示就会
        ///   挤成一整行。编译器不会报错，只有人眼能发现 —— 所以钉在这里。
        #[test]
        fn slint的占位符与换行数量一致() {
            let mut bad = Vec::new();
            for (id, s) in po_entries() {
                if id.matches("{}").count() != s.matches("{}").count() {
                    bad.push(format!("占位符数量不同：{id:?} → {s:?}"));
                }
                if id.matches('\n').count() != s.matches('\n').count() {
                    bad.push(format!("换行数量不同：{id:?} → {s:?}"));
                }
            }
            assert!(bad.is_empty(), "{bad:#?}");
        }
    }
}
