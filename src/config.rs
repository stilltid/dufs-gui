//! 配置持久化：%APPDATA%\dufs-gui\config.json

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 同时接受「一个字符串」和「字符串数组」。
///
/// `bind_ips` 原本是**一个字符串**（`"bind_ip": "10.0.0.5"`），现在是一个数组
/// （`"bind_ips": ["10.0.0.5", "192.168.1.7"]`）。直接用 `Vec<String>` 反序列化会让
/// **老配置文件解析失败**，然后被 `#[serde(default)]` 悄悄换成默认值 ——
/// 用户看到的是「我选的网卡怎么没了」。所以两种形状都收。
/// （dufs 自己对 `hidden` 就是这么干的：`deserialize_string_or_vec`。）
fn de_string_or_seq<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    Ok(match OneOrMany::deserialize(de)? {
        OneOrMany::One(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![s]
            }
        }
        OneOrMany::Many(v) => v.into_iter().filter(|s| !s.trim().is_empty()).collect(),
    })
}

/// 账号列表里的一行 = 一条规则片段。
///
/// 界面上一行只填**一个**路径；同一个用户名写多行 = 它的多个路径，
/// 拼装时（`dufs::build_accounts_args`）会按（用户名，密码）合并成一个 `-a`。
/// 用户名留空 = 匿名访问（多行会自动合并成一条匿名规则）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct AccountRow {
    /// 用户名。**空 = 匿名**。
    pub name: String,
    /// 明文密码，或 `$6$` 开头的哈希原文（原样透传，dufs 自己识别）。
    pub secret: String,
    /// 单个路径，必须以 `/` 开头。
    pub path: String,
    /// 权限。序列化成 `"rw"` / `"ro"`，永远显式写出（不省略，拼出来的命令行一眼能看懂）。
    ///
    /// ★ 类型是枚举不是 `String`：非法值在**加载阶段**就被拒，而且拼命令行只走
    ///   `as_dufs()` —— 界面文案怎么翻都影响不到它。
    pub perm: Perm,
}

/// 「最近用过的共享路径」最多记几条
pub const RECENT_PATHS_CAP: usize = 5;

/// 关闭窗口时的默认动作。`serde` 存小写（"quit" / "tray"）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum CloseAction {
    /// 直接退出：停服务 + 退出程序（默认）
    #[default]
    Quit,
    /// 隐藏到托盘：服务继续跑
    Tray,
}

impl CloseAction {
    /// 界面下拉框的下标（0 = 直接退出，1 = 隐藏到托盘）
    pub fn to_index(self) -> i32 {
        match self {
            CloseAction::Quit => 0,
            CloseAction::Tray => 1,
        }
    }

    /// 下标 → 动作（越界回退到默认）
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => CloseAction::Tray,
            _ => CloseAction::Quit,
        }
    }
}

// ============================================================
// 「模式」类字段一律用枚举
// ============================================================
//
// ★ 为什么（三条）：
//
//   1. **界面文案不再是值**。权限下拉框原来是
//      `selected(v) => { ... v == "只读" ? 1 : 0 }` —— 拿**显示文案**当数据。
//      英文界面下 `"Read-only" != "只读"`，账号权限会被静默写成「读写」。
//      枚举化之后界面传回来的是行号，由 `from_index` 一处换算，文案怎么翻都
//      影响不到它。
//   2. **非法值在加载阶段就被拒**，不再靠 `validate_accounts` 兜底。
//   3. 线格式（`"rw"` / `"none"` / `""` …）是**数据契约**，由各枚举的
//      `as_*` / `from_*` 一处定义，并有单测交叉验证（见文件末尾 `tests`）。

/// 账号权限。`serde` 线格式 `"rw"` / `"ro"`。
///
/// ★ 这是原 §3-1 那个 bug 的**类型级修复**：界面下拉框曾经写成
///   `selected(v) => { root.account-perm-changed(idx, v == "只读" ? 1 : 0) }`，
///   英文界面下 `"Read-only" != "只读"`，权限会被静默写成「读写」。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Perm {
    Rw,
    Ro,
}

/// ★ `Default` 是 **`Ro`**（只读），不是 `Rw`。
///
///   只在**配置里缺 `perm` 字段**时生效（`AccountRow` 有结构体级 `#[serde(default)]`）。
///   老版本这个字段是 `String`，缺失时是空串 → 被 `validate_accounts` 拦下报错；
///   换成枚举后拦不住了，所以取**最保守**的一边：dufs 自己的规矩也是
///   「省略权限 = 只读」，而「字段缺失就把只读账号变成可写」是权限方向上的错，
///   绝不能发生（fail-closed）。
impl Default for Perm {
    fn default() -> Self {
        Perm::Ro
    }
}

impl Perm {
    /// dufs `-a` 的权限后缀：`admin:pw@/:rw` 里的 `rw`。
    ///
    /// ★ 命令行**只准**从这里取权限 —— `dufs.rs` 里不许再出现 `"rw"` 这种字面量。
    pub fn as_dufs(self) -> &'static str {
        match self {
            Perm::Rw => "rw",
            Perm::Ro => "ro",
        }
    }

    /// 线格式 → 枚举。给「解析老原文规则」用（配置文件走 serde，不走这里）。
    pub fn from_dufs(s: &str) -> Option<Self> {
        [Perm::Rw, Perm::Ro].into_iter().find(|p| p.as_dufs() == s)
    }

    /// 界面 ComboBox 的行号 ⇄ 枚举。
    ///
    /// ★ 顺序必须与 `ui/pages/share.slint` 里那个 ComboBox 的 `model: ["读写", "只读"]`
    ///   一致（两处都写了这条注记）。
    ///
    /// ★ 为什么 Slint 侧不用枚举、只用一个 int：ComboBox 只有 `model`（字符串表）
    ///   和 `current-index`（int），没有"把枚举当模型"的能力；而给带**单向绑定**的
    ///   属性赋值会**打断绑定**（`i-slint-core/properties.rs`：`intercept_set_binding`
    ///   默认返回 false = "the binding will simply be removed"，只有两向绑定 `<=>`
    ///   拦得住），ComboBox 内部正是 `self.current-index = index`。所以
    ///   `current-index: Rules.x-to-index(root.x)` 在用户点过一次之后就不跟着
    ///   `root.x` 走了，表现为「恢复默认后下拉框还显示旧值」。
    ///   把行号换算留在 Rust（Slint 那边继续用 `<=>`），两边都成立，
    ///   而且「行号 ⇄ 语义」只有这一个来源。
    ///
    /// 越界一律收到**只读**（fail-closed：宁可少给权限）。
    pub fn to_index(self) -> i32 {
        match self {
            Perm::Rw => 0,
            Perm::Ro => 1,
        }
    }

    pub fn from_index(i: i32) -> Self {
        match i {
            0 => Perm::Rw,
            _ => Perm::Ro,
        }
    }
}

/// `--compress` 级别。`serde` 线格式 `"none"` / `"low"` / `"medium"` / `"high"`。
/// 顺序与 `ui/pages/share.slint` 里的 `model: ["不压缩", "低", "中", "高"]` 一致。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Compress {
    None,
    /// ★ 也是 `Default`：和 dufs 的 `--compress` 默认值一致。
    ///
    /// ★ 兼认空串：老配置里出现过 `"compress": ""`（手改过或更老的版本），
    ///   老代码对空串的处理是「不传 `--compress`」，而 dufs 的默认值就是 `low`，
    ///   所以空串 == low。不认的话整份配置会因为一个字段解析失败被丢回默认值
    ///   （见 `Config::load`：解析失败就 `Config::default()`），代价太大。
    #[default]
    #[serde(alias = "")]
    Low,
    Medium,
    High,
}

impl Compress {
    /// `--compress` 的参数值。
    pub fn as_arg(self) -> &'static str {
        match self {
            Compress::None => "none",
            Compress::Low => "low",
            Compress::Medium => "medium",
            Compress::High => "high",
        }
    }

    /// 行号 ⇄ 枚举（同上：顺序即界面 ComboBox 的行号）。
    pub fn to_index(self) -> i32 {
        match self {
            Compress::None => 0,
            Compress::Low => 1,
            Compress::Medium => 2,
            Compress::High => 3,
        }
    }

    /// 越界回落到 `Low`（与老代码的 `level_of(.., "low")` 一致）。
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => Compress::None,
            2 => Compress::Medium,
            3 => Compress::High,
            _ => Compress::Low,
        }
    }
}

/// 静态站点渲染模式。`serde` 线格式 `""` / `"index"` / `"try-index"` / `"spa"`。
///
/// ★ 空串是**真实取值**（= 都不开，dufs 的默认），所以不能用 `rename_all`，
///   得逐个写 `rename`。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RenderMode {
    #[default]
    #[serde(rename = "")]
    Default,
    #[serde(rename = "index")]
    Index,
    #[serde(rename = "try-index")]
    TryIndex,
    #[serde(rename = "spa")]
    Spa,
}

impl RenderMode {
    /// 对应的 `--render-*` 参数名。`Default`（空串）不传参数，返回 `None`。
    pub fn as_arg(self) -> Option<&'static str> {
        match self {
            RenderMode::Default => None,
            RenderMode::Index => Some("--render-index"),
            RenderMode::TryIndex => Some("--render-try-index"),
            RenderMode::Spa => Some("--render-spa"),
        }
    }

    /// 行号 ⇄ 枚举（顺序与 `ui/pages/advanced.slint` 的 `model` 一致）。
    pub fn to_index(self) -> i32 {
        match self {
            RenderMode::Default => 0,
            RenderMode::Index => 1,
            RenderMode::TryIndex => 2,
            RenderMode::Spa => 3,
        }
    }

    /// 越界回落到 `Default`（与老代码的 `level_of(.., "")` 一致）。
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => RenderMode::Index,
            2 => RenderMode::TryIndex,
            3 => RenderMode::Spa,
            _ => RenderMode::Default,
        }
    }
}

/// 界面语言。`serde` 线格式 `"system"` / `"zh-CN"` / `"en"`。
///
/// ★ 为什么不用 `Language` 这种三个字的名字去拼系统语言：
///   界面文案有两套来源 —— Slint 的 `@tr()`（译文编进 exe）和 Rust 的
///   `i18n::t()`。两边**必须同时切**，否则会出现「界面英文、提示中文」。
///   所以这个枚举是唯一入口，`service::apply_language()` 会同时通知两边。
///
/// ★ **顺序即界面 ComboBox 的行号**（和 `Perm` / `Compress` 一样的约定：
///   行号 ⇄ 语义的换算只写在 Rust 里，Slint 侧只存行号）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Language {
    /// 跟随系统语言（**默认**）。
    ///
    /// ★ 判定规则刻意写得和 Slint 框架**一模一样**（见 i-slint-core 的
    ///   `language_index_from_sys_locale`）：先找完全相等的语言名，再比语言主体，
    ///   而我们的语言包里只有 `""`（中文源串）和 `"en"`，
    ///   所以实际结果就是「`en*` 系统 → 英文，其它一律 → 中文」。
    ///   不能自作主张写成「非中文就英文」：德语系统在框架那边会回落到中文，
    ///   而 Rust 侧如果给英文，界面就会中英混搭。
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en")]
    En,
}

impl Language {
    /// 行号 ⇄ 枚举（顺序即「应用」页那个 ComboBox 的行号）。
    pub fn to_index(self) -> i32 {
        match self {
            Language::System => 0,
            Language::ZhCn => 1,
            Language::En => 2,
        }
    }

    /// 越界回落到 `System`（和「配置里没写这个字段」一致）。
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Language::ZhCn,
            2 => Language::En,
            _ => Language::System,
        }
    }
}

/// `language` 字段的反序列化：**非法值回落到 `System`，绝不报错**。
///
/// ★ 为什么不能直接用 `#[derive(Deserialize)]` 让 serde 拒绝非法值：
///   `Config::load` 遇到解析失败会**整份丢回 `Config::default()`**
///   （见那里的注释）—— 用户手写一个 `"language": "zh"` 就会把
///   共享路径、端口、账号全丢掉。这个代价换来一句"配置格式不对"，
///   完全不成比例。同一个理由让 `Compress` 认下了空串。
///
/// ★ 顺手认下常见的写法：`zh` / `zh-Hans` 这类都算简体中文，
///   `en-US` 这类都算英文。**其它语言一律回落到 `System`**（而不是英文），
///   因为回落到「跟随系统」还能让框架再判断一次，回落到写死的语言则是单向的。
fn de_language<'de, D>(d: D) -> Result<Language, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    // 类型不对（比如写了个数字）也回落，不报错
    let raw = String::deserialize(d).unwrap_or_default();
    Ok(language_from_tag(&raw))
}

/// 语言名 → 枚举。单独拆出来是为了能直接写单测（不用造 JSON）。
fn language_from_tag(raw: &str) -> Language {
    // 只看语言主体（`en-US` → `en`），大小写不敏感
    let base = raw
        .split(['-', '_', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match base.as_str() {
        "zh" => Language::ZhCn,
        "en" => Language::En,
        "system" | "" => Language::System,
        _ => Language::System,
    }
}

/// 界面明暗主题。`serde` 线格式 `"system"` / `"light"` / `"dark"`。
///
/// ★ 名字**故意不叫 `Theme`**：`ui/theme.slint` 里的 `export global Theme`
///   会被 `slint::include_modules!()` 生成一个同名的 Rust 类型 `Theme`，
///   在 `main.rs` / `service.rs` 里 `use` 一下就撞名了。
///
/// ★ **顺序即界面 ComboBox 的行号**（和 `Language` / `Perm` / `Compress` 一样的约定：
///   行号 ⇄ 语义的换算只写在 Rust 里）。
///
/// ★ 它不是 dufs 的选项，是本程序自己的设置 —— 和 `language` / `stay_on_top` 一样
///   **不受「恢复默认设置」影响**（`restored()` 不动它）；否则用户点完恢复，
///   界面会突然变一种配色，而确认框上的「该操作无法撤销」还没看清。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// 跟随系统（**默认**）：Windows 切成深色，界面跟着深。
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    /// 行号 ⇄ 枚举（顺序即「应用 → 外观」那个 ComboBox 的行号）。
    pub fn to_index(self) -> i32 {
        match self {
            ThemeMode::System => 0,
            ThemeMode::Light => 1,
            ThemeMode::Dark => 2,
        }
    }

    /// 越界回落到 `System`（和「配置里没写这个字段」一致）。
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => ThemeMode::Light,
            2 => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }
}

/// `theme_mode` 字段的反序列化：**非法值回落到 `System`，绝不报错**。
///
/// ★ 理由和 `de_language` 一模一样：`Config::load` 解析失败会**整份丢回
///   `Config::default()`**，一个手写的 `"theme_mode": "sepia"` 不该把共享路径、
///   端口、账号一起清掉。
fn de_theme_mode<'de, D>(d: D) -> Result<ThemeMode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    // 类型不对（比如写了个数字）也回落，不报错
    let raw = String::deserialize(d).unwrap_or_default();
    Ok(theme_mode_from_tag(&raw))
}

/// 主题名 → 枚举。单独拆出来是为了能直接写单测（不用造 JSON）。
fn theme_mode_from_tag(raw: &str) -> ThemeMode {
    match raw.trim().to_ascii_lowercase().as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        // "system" / "" / 以及任何不认识的写法一律跟随系统
        _ => ThemeMode::System,
    }
}

/// 应用配置。字段全部带默认值（`#[serde(default)]`），
/// 这样以后新增字段时旧配置文件仍能正常读取。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    // ---------- 服务入口（共享路径 / 端口 / 监听网卡）----------
    /// 共享路径（dufs 的 serve-path）。
    ///
    /// ★ 可以是**文件夹**，也可以是**单个文件** —— dufs 的 serve-path 两者都收，
    ///   单个文件时根路径 `/` 直接返回该文件的内容。
    ///
    /// 字段名原先叫 `folder`（只能选目录的年代）。改名后靠 `alias` 兼容旧的
    /// config.json：旧值能读进来，保存时统一写成新名字。
    #[serde(alias = "folder")]
    pub serve_path: String,
    pub port: u16,
    /// 监听网卡：空 = 0.0.0.0（所有网卡）；否则是具体 IP，对应 `-b <ip>`
    /// 要监听的网卡地址（dufs 的 `-b`，**可以多个**）。
    ///
    /// ★ 空数组 = 「所有网卡」= `-b 0.0.0.0`（本机 IPv6 可用时再加一个 `-b ::`）；
    ///   非空 = 只绑列出的这些地址，一个地址配一个 `-b`。
    ///
    /// ★ 为什么是数组：dufs 的 `-b` 是 append 型（实测 `-b a -b b` 两个都绑上），
    ///   界面上原来只能选一个，等于白扔了 dufs 的能力。
    ///
    /// ★ 兼容旧配置：老版本这里是**一个字符串**，所以用 `de_string_or_seq` 同时收
    ///   两种形状，并保留旧键名 `bind_ip` 作为别名。详见那个函数。
    #[serde(default, deserialize_with = "de_string_or_seq", alias = "bind_ip")]
    pub bind_ips: Vec<String>,

    // ---------- 权限与访问控制 ----------
    pub allow_upload: bool,
    pub allow_search: bool,
    pub allow_delete: bool,
    /// 只读模式：勾选后压制所有 allow-*。
    ///
    /// ★ 它**不是** dufs 的参数（`--help` 里没有任何 readonly / deny 选项），
    ///   只是"把 6 个 `--allow-*` 全部不传"的**快捷开关** —— 和上面的
    ///   「放开全部操作」（全开）正好是一对。用户明确要一个"只读模式"，所以留着。
    pub readonly_mode: bool,
    /// `--allow-archive` 允许把文件夹打包下载
    pub allow_archive: bool,
    /// `--compress` 级别。★ 只在「能打包下载」（`allow_archive` 或 `allow_all`）
    /// 且非只读、非单文件时才会传递（见 `dufs::build_args`）。
    pub compress: Compress,
    /// `--allow-hash` 允许 ?hash 取 sha256
    pub allow_hash: bool,
    /// `--allow-symlink` 允许跟随指向外部的符号链接
    pub allow_symlink: bool,
    /// `--enable-cors`
    pub enable_cors: bool,
    /// `--allow-all` 放开全部操作
    pub allow_all: bool,
    /// `--hidden` 在列表里隐藏的路径，逗号分隔，如 `tmp,*.log,.git`
    pub hidden: String,

    // ---------- 认证（账号列表 → -a）----------
    pub use_auth: bool,
    /// 账号列表（可视化多账号编辑器的真相来源）。
    ///
    /// 替代了老版本的一组"用户名/密码"框 —— 那两个字段现在只做迁移源，
    /// 不再写回文件（见 `migrate_legacy`）。
    /// 为空 = 没有可视化账号（启用与否看 `use_auth` 和原文框）。
    #[serde(default)]
    pub accounts: Vec<AccountRow>,
    // 老"用户名/密码"框：只读迁移源（`skip_serializing`，新配置里不再出现）。
    // 见 `migrate_legacy`。字段保持 pub 只是因为结构体更新语法（`..Config::default()`）
    // 要求所有字段可见 —— 别在新代码里读写它们，读配置只走迁移，写配置只走 accounts。
    #[serde(default, skip_serializing)]
    pub username: String,
    #[serde(default, skip_serializing)]
    pub password: String,
    // 老"专家模式"原文框：只读迁移源（`skip_serializing`，新配置里不再出现）。
    // 原文框 UI 和透传逻辑已经删掉，只剩可视化列表。但老配置文件里可能还存着规则，
    // 加载时由 `migrate_legacy` 转成 `accounts` 行（见 `parse_auth_rules_to_accounts`），
    // 转完清空。字段保持 pub 只是因为结构体更新语法要求所有字段可见 ——
    // 别在新代码里读写它，读配置只走迁移，写配置只走 accounts。
    #[serde(default, skip_serializing)]
    pub auth_rules: String,
    /// `--path-prefix`
    pub path_prefix: String,

    // ---------- 呈现与外部集成（渲染 / TLS / 日志 / 外部文件 / 额外参数）----------
    /// 静态站点模式。`Default` = 都不开。
    pub render_mode: RenderMode,
    pub tls_cert: String,
    pub tls_key: String,
    pub log_file: String,
    pub log_format: String,
    /// 关闭 http 访问日志（对应 dufs 的 `--log-format ''`）
    pub log_disabled: bool,
    /// `--assets` 覆盖内置前端资源的目录。★ 必须含 index.html，否则 dufs 拒绝启动
    pub assets: String,
    /// `-c, --config` dufs 配置文件。★ 实测 0.46 只认 JSON / YAML（TOML 会解析失败）；
    /// 且命令行参数优先于配置文件里的同名项。
    pub dufs_config: String,
    /// 额外命令行参数原文，按空格切分后追加到最前面（位置参数之前）
    pub extra_args: String,

    // ---------- 界面状态 ----------
    /// 点窗口 X 时的默认动作。
    ///
    /// 默认 **Quit**（关了就是关了：停服务 + 退出程序）。
    /// 理由：Windows 用户对右上角 X 的预期就是关掉程序；
    /// 改掉这个语义（关窗后还在后台共享文件）既反直觉，也有安全隐患
    /// （桌面看不到任何迹象，只有托盘里一个小图标）。
    ///
    /// 想让它常驻的人，在「应用」页切到 `Tray`，或打开「关闭窗口前先询问」在关窗时现选。
    pub close_action: CloseAction,
    /// 界面语言。
    ///
    /// ★ 它不是 dufs 的选项，是本程序自己的设置 —— 所以和 `stay_on_top`
    ///   一样**不受「恢复默认设置」影响**（`restored()` 不动它）；
    ///   否则用户点了恢复，界面会突然变成另一种语言，而确认框上的
    ///   「该操作无法撤销」还没看清。
    ///
    /// ★ 它**在启动早期就要用到**（比 `push_cfg_to_ui` 还早）：
    ///   否则窗口先以中文建好、再切成英文，用户会看到一闪。
    ///   见 `main.rs` 里 `apply_language` 的调用位置。
    ///
    /// ★ `deserialize_with` 而不是靠派生：非法值得**回落**而不是报错，
    ///   否则一个手写的 `"zh"` 会让整份配置丢回默认值（见 `de_language`）。
    #[serde(default, deserialize_with = "de_language")]
    pub language: Language,
    /// 界面明暗主题（跟随系统 / 浅色 / 深色）。
    ///
    /// ★ 和 `language` 一样是本程序自己的设置，**不受 `restored()` 影响**。
    /// ★ `deserialize_with` 而不是靠派生：非法值要回落而不是报错（见 `de_theme_mode`）。
    #[serde(default, deserialize_with = "de_theme_mode")]
    pub theme_mode: ThemeMode,
    /// 点窗口 X 时是否先弹框问（默认开）。
    ///
    /// 关掉后关闭窗口直接执行 `close_action`，一步到位。
    /// 询问框里勾了「不再询问」也会关掉它，并把那次选的动作记下来。
    pub confirm_on_close: bool,
    /// 老字段迁移源：0.x 版存的是 `close_to_tray: bool`。
    /// 只读不写（`skip_serializing`），`migrate_legacy` 里转完就清空。
    #[serde(default, rename = "close_to_tray", skip_serializing)]
    pub legacy_close_to_tray: Option<bool>,
    /// 窗口始终置顶（浮在最前面）。
    ///
    /// 默认 **false**。勾上后切到资源管理器找文件时，本窗口不会被盖住，
    /// 拖文件进来特别顺手。不勾则正常层叠。
    /// 选择会被记住，下次启动自动恢复。
    pub stay_on_top: bool,

    /// 登录 Windows 后自动运行本程序（写 HKCU 的 Run 项）。默认关。
    ///
    /// ★ 注册表里存的是**当时那个 exe 的路径**，程序换位置后要重新勾一次。
    /// ★ 它只管"打开程序"，要不要顺便开共享看 `autostart_serve`。
    pub autostart: bool,
    /// 程序启动后自动开始共享。默认关。
    ///
    /// 和 `autostart` 搭配才是真正的"开机就共享"；单独勾上则只对
    /// 手动双击启动生效（相当于启动即服务）。
    ///
    /// ★ 安全前提：没开账号认证时开机静默共享 = 局域网裸奔，
    ///   所以界面上会先弹一次确认（见 `callbacks::on_autostart_serve_changed`）。
    pub autostart_serve: bool,
    /// 开机自启那一次**静默启动**：直接进托盘，不弹窗口。
    ///
    /// ★ 只影响"登录后自动运行"那一次：注册表命令会多带一个 `--tray`。
    ///   手动双击 exe 永远显示窗口（不然用户会以为程序没启动）。
    pub start_in_tray: bool,
    /// 已经因为"没开认证"提醒过一次自动共享的风险（不再反复问）
    pub warned_autostart_no_auth: bool,
    /// 最近用过的共享路径（新的在前，最多 `RECENT_PATHS_CAP` 条）。
    ///
    /// ★ 只影响界面上的"最近"下拉，不会自动改 `serve_path` ——
    ///   路径被人删掉时不该静默把共享指到别的地方。
    pub recent_paths: Vec<String>,

    // ---------- 窗口状态（下次启动恢复）----------
    /// 上次退出时窗口左上角的**物理**像素坐标。
    ///
    /// 存物理坐标是因为显示器摆放本来就是物理像素，不随 DPI 缩放变；
    /// 宽度相反，存**逻辑**像素（见 `window_width`）。
    /// `None` = 还没存过（首次运行），交给 Windows 自己放。
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// 上次退出时窗口的宽度（**逻辑**像素）。
    /// 存逻辑宽度是为了换 DPI 的机器/显示器时窗口看起来一样大。
    pub window_width: Option<f32>,
    /// 窗口高度的**基准**："服务未运行"时的高度（逻辑像素）。
    ///
    /// ★ 它是基准而不是最终值：运行时会在它上面再加 `QR_EXTRA_HEIGHT`
    ///   （"手机访问"那一块），所以既尊重用户拖出来的高度，
    ///   又保留了"开服务就变高"的行为。
    pub window_height: Option<f32>,
    /// 上次退出时窗口是不是最大化的
    pub window_maximized: bool,

    /// 本程序自己的详细日志(`%TEMP%\dufs-gui.log`)。默认开。
    ///
    /// ★ 它与 dufs 的访问日志(`log_file`)是**两回事**:
    ///   那份是 dufs 记的"谁访问了什么",这份是本程序记的"自己干了什么"
    ///   (拼出的命令行、等待就绪、网卡变化、拖放注册……),纯排障用。
    ///   关掉后只保留关键事件,日志短很多。
    pub verbose_log: bool,
    /// 多网卡时用户选定的"显示用"地址（记住选择）。
    ///
    /// ★ 它**只影响二维码 / 复制 / 浏览器打开用哪个地址，不影响 dufs 监听**（
    ///   监听是 `bind_ips`）。两者必须分开：
    ///   - `bind_ips` 对应 dufs 的 `-b`，改了必须重启 dufs → 运行中不能改；
    ///   - `preferred_ip` 只改显示 → 运行中随时可改。
    ///
    /// 第一版界面里的「手机访问 → 网卡」下拉框用的是这个字段；
    /// 后来界面改成分页布局时，那个下拉框被搬去共享页并改成了监听地址
    /// （语义变了），这个字段就成了没人读的死字段 —— 一度被当垃圾删掉。
    /// 现在恢复：它是「运行中切地址」这个功能的落脚点。
    ///
    /// 与 `bind_ips` 不同，这个值失效（网卡不见了）时**不需要清理**：
    /// 它不会导致启动失败，显示时自然回落评分最高的地址；
    /// 而且保留着，网卡回来时选择还能生效。
    pub preferred_ip: String,
    pub firewall_prompted: bool,
    pub warned_no_auth: bool,


    // ---------- 运行期状态（不写进配置文件）----------

    /// 本机 IPv6 是否可用。**不持久化**：它取决于运行环境（换机器、网卡被禁用、
    /// 系统里关掉 IPv6 都会变），存下来只会在下一次启动时骗人。
    ///
    /// 由 `service` 在每次启动前用 `net::ipv6_available()` 探测后写入；
    /// `dufs::build_args` 据此决定「所有网卡」要不要补一个 `-b ::`。
    ///
    /// 默认 `false` —— 这样黄金用例（`Config::default()`）的结果不随运行环境变化。
    ///
    /// 把「运行期推导出来、又会影响参数拼装」的事实放在同一个结构体里，是抄 dufs
    /// 自己的做法（`src/args.rs` 里就有 `#[serde(skip)] pub path_is_file: bool`）。
    #[serde(skip)]
    pub ipv6_available: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            serve_path: default_serve_path(),
            port: 5000,
            bind_ips: Vec::new(),

            allow_upload: true,
            allow_search: true,
            allow_delete: false,
            allow_archive: false,
            compress: Compress::Low,
            allow_hash: false,
            allow_symlink: false,
            enable_cors: false,
            allow_all: false,
            hidden: String::new(),

            use_auth: false,
            accounts: Vec::new(),
            username: String::new(),
            password: String::new(),
            auth_rules: String::new(),
            path_prefix: String::new(),

            render_mode: RenderMode::Default,
            tls_cert: String::new(),
            tls_key: String::new(),
            log_file: String::new(),
            log_format: String::new(),
            log_disabled: false,
            assets: String::new(),
            dufs_config: String::new(),
            extra_args: String::new(),

            close_action: CloseAction::Quit,
            confirm_on_close: true,
            legacy_close_to_tray: None,
            // ★ 默认「跟随系统」而不是写死中文：中文系统上结果还是中文
            //   （主场景不变），英文系统上开箱就是英文。
            //   注意它**不是** dufs 的选项，所以不受 `restored()` 影响。
            language: Language::System,
            // 默认跟随系统：现代应用的通行做法，也不改变"中文系统上开箱即中文"的逻辑。
            theme_mode: ThemeMode::System,
            stay_on_top: false,
            autostart: false,
            autostart_serve: false,
            start_in_tray: false,
            warned_autostart_no_auth: false,
            recent_paths: Vec::new(),
            window_x: None,
            window_y: None,
            window_width: None,
            window_height: None,
            window_maximized: false,
            verbose_log: true,
            preferred_ip: String::new(),
            firewall_prompted: false,
            warned_no_auth: false,

            readonly_mode: false,

            // 运行期探测，默认 false
            ipv6_available: false,
        }
    }
}

/// 校验高级认证规则（`-a`）的写法。
///
/// 不校验的话，写错了要等 dufs 启动失败才看得到，而且它的报错长这样：
/// ```text
/// Error: Invalid auth value `admin:pw@/:r
/// ```
/// （连反引号都没闭合），对普通用户完全没有指导意义。
///
/// 语法（依据 dufs 官方 README，已在 0.46.0 实测）：
///   `[用户名:密码@]路径[:rw|:ro][,路径...]`，多个账号用 `;` 分隔（本程序自己拆成多个 -a）。
///   ★ 只读是 `:ro`，或者直接省略；**没有 `:r`**。
/// 校验结果里的**一个**问题。
///
/// ★ 为什么是结构不是字符串：自检（`--selftest-options`）要断言"哪一行、哪一类问题"，
///   而报错文案是要翻译的。把「**是什么问题**」和「**怎么说**」分开之后：
///   - 断言与语言无关：`p.index == 0 && p.kind == AccountProblemKind::NameHasSyntaxChar('@')`；
///   - 显示层调 `p.describe()` —— 从 Step 5 起它内部走 i18n，断言那边一个字都不用改。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AccountProblem {
    /// **0 基**的行号（和界面列表、`accounts` 数组的次序一致）。给人看时 +1。
    pub index: usize,
    pub kind: AccountProblemKind,
}

/// 问题分类。带参数的几种把**用户数据**（用户名/路径）一起带上，
/// 这样 `describe()` 能拼出完整句子，调用方不用回头再查一遍配置。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AccountProblemKind {
    /// 匿名行（用户名留空）却填了密码
    AnonymousWithSecret,
    /// 用户名含规则语法字符（会破坏 `-a` 的解析）
    NameHasSyntaxChar(char),
    NameHasSpace,
    SecretMissing,
    SecretHasSyntaxChar(char),
    SecretHasSpace,
    /// 形似哈希（`$...$`）但不是 dufs 认的 `$6$`
    UnsupportedHash,
    /// 同一个用户名写了不同密码。`other` 是**另一个行号**（0 基）
    NameConflictingSecret {
        other: usize,
        name: String,
    },
    PathMissing,
    /// 路径不以 `/` 开头
    PathNotAbsolute {
        path: String,
    },
    PathHasSpace,
    PathHasSyntaxChar(char),
}

impl AccountProblem {
    /// 给人看的文案。★ **这里就是显示层** —— Step 5 起把这些字面量搬进 i18n catalog，
    /// 上面那些断言一行都不用改。
    pub fn describe(&self) -> String {
        use crate::i18n::{K, tn};

        // 对外是 1 基行号（用户数行从 1 开始），内部一律 0 基
        let no = self.index + 1;
        match &self.kind {
            AccountProblemKind::AnonymousWithSecret => tn(K::AccAnonymousWithSecret, &[&no]),
            AccountProblemKind::NameHasSyntaxChar(ch) => tn(K::AccNameSyntaxChar, &[&no, ch]),
            AccountProblemKind::NameHasSpace => tn(K::AccNameSpace, &[&no]),
            AccountProblemKind::SecretMissing => tn(K::AccSecretMissing, &[&no]),
            AccountProblemKind::SecretHasSyntaxChar(ch) => tn(K::AccSecretSyntaxChar, &[&no, ch]),
            AccountProblemKind::SecretHasSpace => tn(K::AccSecretSpace, &[&no]),
            AccountProblemKind::UnsupportedHash => tn(K::AccUnsupportedHash, &[&no]),
            AccountProblemKind::NameConflictingSecret { other, name } => {
                tn(K::AccConflictingSecret, &[&no, &(other + 1), name])
            }
            AccountProblemKind::PathMissing => tn(K::AccPathMissing, &[&no]),
            AccountProblemKind::PathNotAbsolute { path } => {
                tn(K::AccPathNotAbsolute, &[&no, path])
            }
            AccountProblemKind::PathHasSpace => tn(K::AccPathSpace, &[&no]),
            AccountProblemKind::PathHasSyntaxChar(ch) => tn(K::AccPathSyntaxChar, &[&no, ch]),
        }
    }
}

/// 小助手：少写点样板
fn problem(index: usize, kind: AccountProblemKind) -> AccountProblem {
    AccountProblem { index, kind }
}

/// 字符串里第一个命中的语法字符（`None` = 都没有）
fn first_syntax_char(s: &str, chars: &[char]) -> Option<char> {
    chars.iter().copied().find(|c| s.contains(*c))
}

/// 校验可视化账号列表。返回**全部**问题（空 = 没问题）。
///
/// 为什么必须带行号：一条坏规则会让 dufs 整个起不来
/// （`Error: Invalid auth value` 直接退出），必须让用户知道是哪一行。
/// `index` 是 **0 基**的（和界面列表、`accounts` 数组一致），`describe()` 里 +1 给人看。
///
/// （`Config::validate` 只显示第一条 —— 那是给提示栏用的，行为与以前一致。）
pub fn validate_accounts(rows: &[AccountRow]) -> Vec<AccountProblem> {
    let mut problems = Vec::new();
    // 用户名 -> (密码, 首次出现的行号)：同名必须同密码，否则拼出来的 -a 是错的
    let mut seen: Vec<(String, String, usize)> = Vec::new();

    for (i, r) in rows.iter().enumerate() {
        let name = r.name.trim();
        let secret = r.secret.trim();
        let path = r.path.trim();

        // 空行（刚点"添加"还没填）：用户名、密码、路径全空才算，直接跳过。
        // 如果全是这种空行，外层的"到底有没有账号"检查会拦住，不会悄悄放过。
        if name.is_empty() && secret.is_empty() && path.is_empty() {
            continue;
        }

        if name.is_empty() {
            // 匿名行：密码必须空 —— 匿名没有密码，填了也传不出去，只会让人误会。
            if !secret.is_empty() {
                problems.push(problem(i, AccountProblemKind::AnonymousWithSecret));
            }
        } else {
            // 用户名里不能有规则语法字符（否则拼出来的 -a 会被 dufs 拒绝，
            // 而且 dufs 是"整段拒绝"，一个坏账号会连累所有账号都起不来）。
            // 特别注意冒号：user:pass 是按**第一个**冒号切的，用户名里有冒号会直接认错人
            // （`us:er:pw` → 用户名变成 `us`）。
            if let Some(ch) = first_syntax_char(name, &['@', ':', ',', ';', '|']) {
                problems.push(problem(i, AccountProblemKind::NameHasSyntaxChar(ch)));
            }
            if name.contains(char::is_whitespace) {
                problems.push(problem(i, AccountProblemKind::NameHasSpace));
            }
            if secret.is_empty() {
                problems.push(problem(i, AccountProblemKind::SecretMissing));
            }
            // 密码里 `@ , ; |` 和空格不能有（会破坏 -a 的解析，静默配错最坑）。
            // ★ 但冒号**可以**：dufs 按第一个冒号切 user:pass（HTTP Basic 本来就这么定），
            //   实测 `user:p:a@/` 用密码 `p:a` 能正常登录。
            //   （`,;` 在可视化列表里其实也能传过去，因为一行就是一个独立的 -a；
            //    但原文框是按它们拆账号的 —— 两边不一致不如都不放，保持"两道门行为一致"。）
            if let Some(ch) = first_syntax_char(secret, &['@', ',', ';', '|']) {
                problems.push(problem(i, AccountProblemKind::SecretHasSyntaxChar(ch)));
            }
            if secret.contains(char::is_whitespace) {
                problems.push(problem(i, AccountProblemKind::SecretHasSpace));
            }
            // ★ 哈希陷阱：看起来像哈希、但不是 $6$ 开头的，dufs 会把它当**明文**。
            //   （实测：填 `$5$...` 能"正常工作"，但密码其实就是那串哈希文本。）
            //   这几乎一定是填错了，直接拦住比放过去好。
            if secret.starts_with('$') && !secret.starts_with("$6$") {
                problems.push(problem(i, AccountProblemKind::UnsupportedHash));
            }
            // 同一个用户名写了多行 = 它的多个路径：密码必须相同
            match seen.iter().find(|(n, _, _)| n == name) {
                Some((_, prev, first)) if prev != secret => {
                    problems.push(problem(
                        i,
                        AccountProblemKind::NameConflictingSecret {
                            other: *first,
                            name: name.to_string(),
                        },
                    ));
                }
                Some(_) => {}
                None => seen.push((name.to_string(), secret.to_string(), i)),
            }
        }

        if path.is_empty() {
            problems.push(problem(i, AccountProblemKind::PathMissing));
        } else {
            // dufs 的授权路径是 URL 路径，一定以 `/` 开头
            if !path.starts_with('/') {
                problems.push(problem(
                    i,
                    AccountProblemKind::PathNotAbsolute {
                        path: path.to_string(),
                    },
                ));
            }
            if path.contains(char::is_whitespace) {
                problems.push(problem(i, AccountProblemKind::PathHasSpace));
            }
            // 路径里的语法字符：每个都会静默破坏规则（不断言就白写了校验）。
            //   `:` → 和权限后缀混淆（`/:r` 会被当成"路径 / + 权限 r"，dufs 直接拒启动）；
            //   `,` → dufs 按逗号拆"同一账号的多个路径"，一条变两条；
            //   `|` → dufs 按它拆多个 -a，一条变多条；
            //   `@` → 和 `@/` 切分混淆；`;` → 和原文分隔符混淆（两道门行为一致）。
            // （`/` 当然可以有，它就是路径分隔符。）
            if let Some(ch) = first_syntax_char(path, &[':', ',', '|', '@', ';']) {
                problems.push(problem(i, AccountProblemKind::PathHasSyntaxChar(ch)));
            }
        }
        // 权限不在这里校：它是枚举，非法值在**加载阶段**就被 serde 拒了
        // （见文件末尾的 `非法枚举值被拒` 单测）。
    }
    problems
}

/// 把老原文框的规则解析成账号行。纯函数，不失败（infallible）。
///
/// 只处理**曾经通过原文校验**的输入（存下来的都是结构合法的）。
/// 转出来的行可能本身不合格（比如 `admin@/path` 这种没密码的死规则，
/// 会转成空密码的行），但那不要紧 —— `validate_accounts` 会在启动前
/// 拦下来报错，而且行是**可见的**（用户在界面上直接改，不用手改 JSON）。
/// 唯一"合并不了"的是同名不同密码（`a:pw1@/x;a:pw2@/y`），会原样转成
/// 多行然后被校验拦下（不静默丢，错得明明白白）。
pub fn parse_auth_rules_to_accounts(raw: &str) -> Vec<AccountRow> {
    let mut rows = Vec::new();
    // 原文框是按 `;` / `|` 拆账号的（dufs 自己只认 `|`，`;` 是本程序加的分隔符）。
    for group in raw.split([';', '|']) {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        // dufs 用 `@/` 切账号和路径（见参数清单 §7）。存下来的规则一定有 `@`，
        // 万一没有（手改 JSON 改坏了）就跳过这一组 —— 总比整个起不来强，
        // 而且这种情况校验那关本来也过不了。
        let Some((account, paths)) = group.split_once('@') else {
            continue;
        };
        // 账号用**第一个**冒号切用户名和密码（dufs 和 HTTP Basic 都是这个规矩，
        // 所以密码里可以带冒号，用户名里不行）。没冒号（`admin@/path` 死规则）→
        // 用户名照收、密码留空，转出来的行会被 validate_accounts 拦下。
        let (name, secret) = if account.is_empty() {
            (String::new(), String::new())
        } else {
            match account.split_once(':') {
                Some((u, p)) => (u.to_string(), p.to_string()),
                None => (account.to_string(), String::new()),
            }
        };
        // 同一账号的多个路径用逗号分隔（dufs 语法），一行一个转成多行。
        for one in paths.split(',') {
            let one = one.trim();
            if one.is_empty() {
                continue;
            }
            // 权限是最后一个冒号后面的 rw/ro（路径里不会有冒号，原文校验保证过）；
            // 省略 = 只读（dufs 规矩）。
            // 尾巴不是 rw/ro（比如老原文里的 `:r`）：整段当路径，权限回落到只读，
            // 随后会被 `validate_accounts` 的「路径不能含 `:`」拦下 —— 错得明明白白，
            // 不静默丢。
            let (path, perm) = match one.rsplit_once(':') {
                // 尾巴是 rw/ro → 就是权限；否则整段当路径、权限回落只读
                Some((pp, tail)) => match Perm::from_dufs(tail) {
                    Some(p) => (pp.to_string(), p),
                    None => (one.to_string(), Perm::Ro),
                },
                None => (one.to_string(), Perm::Ro),
            };
            rows.push(AccountRow {
                name: name.clone(),
                secret: secret.clone(),
                path,
                perm,
            });
        }
    }
    rows
}

/// 默认共享路径：优先"桌面"（目录），取不到就退回家目录，再不行用当前目录
fn default_serve_path() -> String {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let desktop = PathBuf::from(&profile).join("Desktop");
        if desktop.is_dir() {
            return desktop.to_string_lossy().into_owned();
        }
        if PathBuf::from(&profile).is_dir() {
            return profile;
        }
    }
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 共享路径的形态。界面据此决定哪些选项有意义（详见 `Config::validate` 与
/// `service::validate_serve_path`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServePathKind {
    /// 空、不存在，或者存在但既不是文件也不是文件夹（例如设备名）
    Invalid,
    /// 文件夹：dufs 的常规用法
    Dir,
    /// 单个文件：dufs 会把它的内容挂在根路径 `/` 上
    File,
}

/// 判断共享路径的形态。纯函数，方便直接写断言。
///
/// 用 `exists()` + `is_dir()` / `is_file()` 而不是只看扩展名：
/// 无扩展名的文件（`Makefile`、`LICENSE`）也要能共享。
pub fn serve_path_kind(path: &str) -> ServePathKind {
    let path = path.trim();
    if path.is_empty() {
        return ServePathKind::Invalid;
    }
    let p = std::path::Path::new(path);
    if p.is_dir() {
        ServePathKind::Dir
    } else if p.is_file() {
        ServePathKind::File
    } else {
        // 不存在，或者是既非文件也非目录的东西（页文件、设备等）
        ServePathKind::Invalid
    }
}

/// 去掉 Windows 的 verbatim 前缀（`\\?\`，UNC 形式是 `\\?\UNC\`）。
///
/// `std::fs::canonicalize` 在 Windows 上一律返回带这个前缀的路径。它会把 Win32 的
/// **路径归一化整个关掉**（`/` 不再当分隔符、`.`/`..` 不再折叠、结尾的空格和点不再被
/// 吃掉），存下来反而更容易踩坑；显示给用户也难看。所以统一去掉。
///
/// 不给「超长路径」开特例：Rust 的 `std::fs` 在路径过长时会**自己在内部**把前缀加回去，
/// dufs 自己也会再 canonicalize 一次。实测 334 字符的目录，去掉前缀后本程序的
/// `is_dir()` 和 dufs 的服务都正常。
fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy();
    // `\\?\UNC\server\share` 的正确还原形式是 `\\server\share`
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s.into_owned(),
    }
}

/// 这个路径**实际**会被共享到哪里 —— 也就是 dufs 最终会用的那个根路径。
///
/// 为什么要自己算一遍（dufs 源码侧的事实）：`Args::sanitize_path` 是
/// `env::current_dir().and_then(|mut p| { p.push(path); std::fs::canonicalize(p) })`，
/// 也就是两件事：
///   1. **相对路径按它自己的工作目录解析**；
///   2. **canonicalize** —— 于是符号链接会被解析成目标。
///
/// 而 dufs 是本程序 spawn 出来的子进程，启动时**没有**给它指定工作目录
/// （`dufs.rs` 里 `Command::new(exe)` 之后没有 `.current_dir(...)`），它继承的就是本进程
/// 的工作目录。所以这里算出来的，**就是** dufs 一定会算出来的那一个。
///
/// 这解决两个用户看不见的问题：
///   - 填 `files` 这种相对路径时，实际共享的是哪儿**取决于程序怎么被启动的**
///     （双击 exe / 快捷方式的「起始位置」/ 命令行的当前目录，三者可以完全不同）；
///   - 选的是符号链接时，dufs 的根是**链接的目标**，`--allow-symlink` 判断
///     「在不在根外面」也是按目标算的。
///
/// 路径无效（不存在 / 既不是文件也不是文件夹 / 没权限）时返回 `None` —— 那种情况界面
/// 另有红字提示，不该在这里再插一句。
pub fn real_serve_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    if !(p.is_dir() || p.is_file()) {
        return None;
    }
    std::fs::canonicalize(p).ok().map(|c| strip_verbatim(&c))
}

/// 界面上「→ 实际共享」那一行要显示什么；不需要显示时返回 `None`。
///
/// 只在**解析结果和用户填的不是同一处**时才显示，否则纯噪音。这些等价写法不显示：
///   - 结尾多个分隔符：`C:\a\b\` 与 `C:\a\b`
///   - 正反斜杠混用、大小写不同（Windows 路径本来就不区分大小写）
pub fn serve_path_hint(path: &str) -> Option<String> {
    let real = real_serve_path(path)?;
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
    if norm(path.trim()) == norm(&real) {
        None
    } else {
        Some(real)
    }
}

/// 拖进来的一组路径 → 最终采用哪一个。
///
/// 规则（开工前定好的，不许猜）：
///   1. 不是正好 1 个 → 拒绝。dufs 一次只能接一个 serve-path，
///      「取第一个」会共享错东西，「全拒绝」最安全；
///   2. 那一个不存在 / 既不是文件也不是文件夹 → 拒绝；
///   3. 否则采用（文件和文件夹都行，见 `serve_path_kind`）。
///
/// 纯函数：`--selftest-options` 里直接拿合成路径测，不用真的去拖文件。
pub fn decide_dropped_path(paths: Vec<PathBuf>) -> Result<PathBuf, String> {
    use crate::i18n::{self, K};
    if paths.is_empty() {
        return Err(i18n::t(K::DropUnrecognized).to_string());
    }
    if paths.len() > 1 {
        return Err(i18n::tn(K::DropTooMany, &[&paths.len()]));
    }
    let p = &paths[0];
    if serve_path_kind(&p.to_string_lossy()) == ServePathKind::Invalid {
        return Err(i18n::tn(K::DropBadPath, &[&p.display()]));
    }
    Ok(p.clone())
}

/// 配置目录：%APPDATA%\dufs-gui
pub fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("dufs-gui")
}

/// 配置文件的完整路径。界面上的「关于」要显示它。
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// 恢复默认时用哪一套基准。
///
/// ★ 为什么要有两套:它们只差两个字段,但结果天差地别 ——
///   一个点完还能用手机传文件,一个点完就变成纯只读。
///   dufs 本身出厂是**纯只读**的;本程序为了主场景故意把上传/搜索打开了。
///   与其替用户选一套,不如两个按钮都摆出来。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Baseline {
    /// 本程序出厂默认:允许上传 + 允许搜索
    AppDefaults,
    /// dufs 原生默认:所有权限关掉 = 纯只读
    DufsNative,
}

impl Config {
    /// 算出一份「恢复默认后」的配置。**纯函数**(不改自己,方便直接写断言)。
    ///
    /// 重置范围(用户定的):
    ///   - 所有 dufs 选项(权限/认证/渲染/TLS/日志/资源/额外参数)
    ///   - 服务入口:端口 → 5000、监听网卡 → 所有网卡
    ///   - **共享路径不动** —— 清掉它界面上就没东西可分享了
    ///   - 本软件自己的设置(置顶 / 关闭行为 / 详细日志 / 开机自启)也不动
    pub fn restored(&self, baseline: Baseline) -> Config {
        let d = Config::default();
        // 从当前配置出发 → 默认什么都不动(共享路径、应用设置就会被保留)
        let mut c = self.clone();

        // ---- dufs 的权限与访问控制 ----
        c.allow_all = d.allow_all;
        c.allow_upload = d.allow_upload;
        c.allow_delete = d.allow_delete;
        c.allow_search = d.allow_search;
        c.allow_archive = d.allow_archive;
        c.allow_hash = d.allow_hash;
        c.allow_symlink = d.allow_symlink;
        c.readonly_mode = d.readonly_mode;
        c.compress = d.compress;
        c.enable_cors = d.enable_cors;
        c.hidden = d.hidden;
        c.use_auth = d.use_auth;
        c.accounts = d.accounts.clone();
        c.username.clear();
        c.password.clear();
        c.auth_rules = d.auth_rules;
        c.path_prefix = d.path_prefix;

        // ---- 呈现与外部集成 ----
        c.render_mode = d.render_mode;
        c.tls_cert = d.tls_cert;
        c.tls_key = d.tls_key;
        c.log_file = d.log_file;
        c.log_format = d.log_format;
        c.log_disabled = d.log_disabled;
        c.assets = d.assets;
        c.dufs_config = d.dufs_config;
        c.extra_args = d.extra_args;

        // ---- 服务入口(端口/网卡回到默认;共享路径保留) ----
        c.port = d.port;
        c.bind_ips = d.bind_ips.clone();

        // ---- dufs 原生默认:把权限也关掉 = 纯只读 ----
        if baseline == Baseline::DufsNative {
            c.allow_upload = false;
            c.allow_search = false;
        }

        c
    }
}

/// 把一条路径插到「最近使用」的最前面（去重、限长）。
///
/// 纯函数，方便直接写断言；调用方负责存盘。
pub fn remember_recent_path(recent: &mut Vec<String>, path: &str) {
    let path = path.trim();
    if path.is_empty() {
        return;
    }
    recent.retain(|p| !p.eq_ignore_ascii_case(path));
    recent.insert(0, path.to_string());
    recent.truncate(RECENT_PATHS_CAP);
}

impl Config {
    /// 读取配置；文件不存在或解析失败都退回默认值（不让用户卡在启动阶段）
    pub fn load() -> Self {
        match std::fs::read_to_string(config_path()) {
            Ok(text) => {
                let mut cfg: Self = serde_json::from_str(&text).unwrap_or_else(|e| {
                    flog!("配置文件解析失败，使用默认配置: {e}");
                    Config::default()
                });
                cfg.migrate_legacy();
                cfg
            }
            Err(_) => Config::default(),
        }
    }

    /// 把老配置里已经换代的字段迁移成等价的新字段（幂等，可反复调）。
    ///
    /// 目前只有一项：老"用户名/密码"框 → 账号列表的首行。
    /// 只在"列表是空的、但老框里有用户名"时做一次；做完就把老框清空，
    /// 所以第二次进来时 `username` 已空，不会再触发。
    pub fn migrate_legacy(&mut self) {
        // 先迁原文框：它以前是**覆盖**列表的（非空时列表直接被忽略），
        // 所以转出来的行替换掉现有列表 —— 那些行以前本来就没生效过。
        if !self.auth_rules.trim().is_empty() {
            let rows = parse_auth_rules_to_accounts(&self.auth_rules);
            flog!(
                "旧配置里的专家模式规则已转为账号列表（{} 行），原文框已移除",
                rows.len()
            );
            self.accounts = rows;
            self.auth_rules.clear();
        }
        if self.accounts.is_empty() && !self.username.trim().is_empty() {
            // 老单框的权限语义：只读模式下是 :ro，否则 :rw（见 build_args 的历史逻辑）
            let perm = if self.readonly_mode { Perm::Ro } else { Perm::Rw };
            self.accounts.push(AccountRow {
                name: self.username.trim().to_string(),
                secret: self.password.clone(),
                path: "/".to_string(),
                perm,
            });
            self.username.clear();
            self.password.clear();
            flog!("旧配置里的用户名/密码已迁移为账号列表的首行");
        }
        // 老“关闭时改为隐藏到托盘”勾 → 新的默认动作（只转一次，转完清空）
        if let Some(to_tray) = self.legacy_close_to_tray.take() {
            self.close_action = if to_tray { CloseAction::Tray } else { CloseAction::Quit };
            flog!("旧配置里的关闭行为已迁移为默认动作");
        }
        // 把当前共享路径补进「最近使用」：升级上来的老配置里这个列表是空的，
        // 而列表空的时候界面整行不显示 —— 用户就永远看不到这个功能。
        // 当前路径本来就是"最近用过的那一个"，补上既真实又让它能被发现。
        if !self.serve_path.trim().is_empty() {
            remember_recent_path(&mut self.recent_paths, &self.serve_path.clone());
        }
    }

    /// 保存配置。失败只打日志，不打断用户操作。
    pub fn save(&self) {
        let dir = config_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            flog!("无法创建配置目录 {}: {e}", dir.display());
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(config_path(), text) {
                    flog!("写入配置失败: {e}");
                }
            }
            Err(e) => flog!("序列化配置失败: {e}"),
        }
    }

    /// 启动前的合法性检查。返回 Err 时把原因直接显示给用户。
    pub fn validate(&self) -> Result<(), String> {
        use crate::i18n::{K, t, tn};

        if self.serve_path.trim().is_empty() {
            return Err(t(K::VldServePathEmpty).to_string());
        }
        // ★ 这里必须同时接受文件和文件夹 —— dufs 的 serve-path 本来就两种都收。
        //   早先只判 `is_dir()`，等于把「共享一个文件」这个能力堵在了界面外。
        if serve_path_kind(&self.serve_path) == ServePathKind::Invalid {
            return Err(tn(K::VldServePathMissing, &[&self.serve_path.trim()]));
        }
        if self.port == 0 {
            return Err(t(K::VldPortInvalid).to_string());
        }
        // TLS 必须成对出现，否则 dufs 会直接报错退出
        let has_cert = !self.tls_cert.trim().is_empty();
        let has_key = !self.tls_key.trim().is_empty();
        if has_cert != has_key {
            return Err(t(K::VldTlsPair).to_string());
        }
        // 成对指定还不够，路径得真的存在 —— 否则用户要等 dufs 报一句英文错误
        // （其余文件类选项 -c / --assets 都做了存在性校验，保持一致）
        if has_cert {
            // 证书 / 私钥分成两个 key —— 文案不同，不该靠拼 label 拼出来
            for (k, path) in [
                (K::VldTlsCertMissing, &self.tls_cert),
                (K::VldTlsKeyMissing, &self.tls_key),
            ] {
                if !std::path::Path::new(path.trim()).is_file() {
                    return Err(tn(k, &[&path.trim()]));
                }
            }
        }
        // ★ 勾了「启用账号访问」，就必须真有账号（原文规则或可视化列表二选一）。
        //   否则界面显示"已启用认证"、实际完全开放（默认还允许上传），
        //   而"未开启认证"的提醒也因 use_auth==true 被跳过，用户一点提示都得不到。
        //   （以前这里查的是"用户名框空不空"；现在看"有没有实质内容"。）
        // "有实质内容" = 用户名、密码、路径三者至少填了一个。
        // 全空的行是"刚点添加还没填"，不算数（validate_accounts 里会直接跳过它）。
        let has_visual = self.accounts.iter().any(|r| {
            !r.name.trim().is_empty() || !r.secret.trim().is_empty() || !r.path.trim().is_empty()
        });
        if self.use_auth && !has_visual {
            return Err(t(K::VldAuthNoAccount).to_string());
        }
        if self.use_auth {
            // ★ 只报第一条（提示栏放不下更多，行为与以前一致）；
            //   完整列表在 `validate_accounts` 的返回值里，自检就是看那个。
            if let Some(p) = validate_accounts(&self.accounts).first() {
                return Err(p.describe());
            }
        }
        // dufs 配置文件：先看存不存在，再看扩展名（0.46 实测不支持 TOML）
        if !self.dufs_config.trim().is_empty() {
            let path = std::path::Path::new(self.dufs_config.trim());
            if !path.is_file() {
                return Err(tn(K::VldDufsConfigMissing, &[&self.dufs_config]));
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(ext.as_str(), "json" | "yaml" | "yml") {
                return Err(tn(K::VldDufsConfigExt, &[&ext]));
            }
        }
        // --assets：dufs 要求这个目录里必须有 index.html，否则启动即失败。
        // 提前拦住，比让用户看到 dufs 的英文报错友好得多。
        if !self.assets.trim().is_empty() {
            let dir = std::path::Path::new(self.assets.trim());
            if !dir.is_dir() {
                return Err(tn(K::VldAssetsMissing, &[&self.assets]));
            }
            if !dir.join("index.html").is_file() {
                return Err(tn(K::VldAssetsNoIndex, &[&self.assets]));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! 配置文件是**唯一会碰用户数据的格式**，所以这里锁三件事：
    //!   1. **旧配置必须逐字能读** —— 枚举化最容易在这里静默改坏东西：
    //!      某个 `rename` 写错一个字母 = 那个字段回落默认值，且**不报错**；
    //!      更糟的是 `Config::load` 遇到解析失败会整份丢回默认值，用户的配置全没了。
    //!   2. 线格式（`as_*` / `as_arg`）与 serde 写出去的值一致 ——
    //!      否则「代码里的字面量」和「配置文件里的值」会各说各话。
    //!   3. 行号 ⇄ 枚举的往返与越界回退（界面 ComboBox 传回来的是行号）。

    use super::*;

    /// 冻结的旧格式配置。
    ///
    /// ★ 故意写**字面量 JSON** 而不是 `serde_json::to_string(&Config::default())` ——
    ///   后者是"自己验自己"，改坏了也测不出来。这里面塞的都是真实存在过的老形状：
    ///   `folder`（旧字段名）、`bind_ips` 是**字符串**（旧类型）、
    ///   `compress` 是空串、`close_to_tray`（旧字段）、老 username/password/auth_rules。
    ///
    /// ★ 路径故意用正斜杠（Windows 认）：反斜杠在 JSON 里要写两个，
    ///   而这条 fixture 要的是「旧字段名/旧类型/枚举线格式」三件事，
    ///   没必要把自己绕进转义的可读性里。
    const LEGACY_JSON: &str = r#"{
      "folder": "C:/share",
      "port": 5000,
      "bind_ips": "10.0.0.5",
      "compress": "",
      "render_mode": "try-index",
      "close_action": "tray",
      "close_to_tray": true,
      "accounts": [
        {"name": "admin", "secret": "pw", "path": "/", "perm": "rw"},
        {"name": "", "secret": "", "path": "/pub", "perm": "ro"}
      ],
      "username": "old",
      "password": "oldpw",
      "auth_rules": "guest:gp@/pub:ro"
    }"#;

    #[test]
    fn 旧格式配置逐字解析() {
        let cfg: Config = serde_json::from_str(LEGACY_JSON).expect("旧配置必须能读");
        // 旧字段名 folder → serve_path
        assert_eq!(cfg.serve_path, "C:/share");
        // 旧类型：bind_ips 是一个字符串
        assert_eq!(cfg.bind_ips, vec!["10.0.0.5"]);
        // ★ 空串 compress 认成 Low（dufs 的默认值就是 low）
        assert_eq!(cfg.compress, Compress::Low);
        assert_eq!(cfg.render_mode, RenderMode::TryIndex);
        assert_eq!(cfg.close_action, CloseAction::Tray);
        // 枚举字段一个都不能错位
        assert_eq!(cfg.accounts[0].perm, Perm::Rw);
        assert_eq!(cfg.accounts[1].perm, Perm::Ro);
        // 老字段仍然读得进来（migrate_legacy 的迁移源）
        assert_eq!(cfg.username, "old");
        assert_eq!(cfg.auth_rules, "guest:gp@/pub:ro");
    }

    /// 线格式与 serde 一致：`as_*()` 返回的字符串必须就是 serde 写出去的那个。
    ///
    /// ★ 这两个来源（`#[serde(rename...)]` 与手写的 `as_arg`）不共享代码，
    ///   所以只能用测试把它们钉在一起。
    #[test]
    fn 线格式与serde一致() {
        for (p, s) in [(Perm::Rw, "rw"), (Perm::Ro, "ro")] {
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{s}\""));
            assert_eq!(Perm::from_dufs(s), Some(p), "{s} 应当能解析回去");
        }
        for (c, s) in [
            (Compress::None, "none"),
            (Compress::Low, "low"),
            (Compress::Medium, "medium"),
            (Compress::High, "high"),
        ] {
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{s}\""));
            assert_eq!(c.as_arg(), s);
        }
        for (m, s) in [
            (RenderMode::Default, ""),
            (RenderMode::Index, "index"),
            (RenderMode::TryIndex, "try-index"),
            (RenderMode::Spa, "spa"),
        ] {
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{s}\""));
        }
        // render_mode 的**参数名**与线格式不同：空串不传参数
        assert_eq!(RenderMode::Default.as_arg(), None);
        assert_eq!(RenderMode::Spa.as_arg(), Some("--render-spa"));
        // Perm 的线格式与命令行后缀恰好同名（这里只是把这件事写下来）
        assert_eq!(Perm::Rw.as_dufs(), "rw");
        // 界面语言：线格式是 `system` / `zh-CN` / `en`
        for (l, s) in [
            (Language::System, "system"),
            (Language::ZhCn, "zh-CN"),
            (Language::En, "en"),
        ] {
            assert_eq!(serde_json::to_string(&l).unwrap(), format!("\"{s}\""));
            assert_eq!(serde_json::from_str::<Language>(&format!("\"{s}\"")).unwrap(), l);
        }
    }

    /// 界面语言的三件事：缺字段/非法值回落、行号往返、**不受「恢复默认设置」影响**。
    ///
    /// ★ 最后一条是这里最要紧的：语言是「本程序自己的设置」，不是 dufs 选项。
    ///   如果哪天有人把它加进 `restored()`，用户点完恢复会发现界面
    ///   突然变成另一种语言，而确认框上那句「该操作无法撤销」还没看清。
    #[test]
    fn 界面语言的行号与回落() {
        // 老配置里没有这个字段 → 不能因此解析失败（否则整份配置丢回默认值）
        let cfg: Config = serde_json::from_str(r#"{"port":5000}"#).expect("缺字段也要能读");
        assert_eq!(cfg.language, Language::System);
        // 非法值同样回落（不弹窗、不报错）
        let cfg: Config = serde_json::from_str(r#"{"language":"de"}"#).expect("非法值回落");
        assert_eq!(cfg.language, Language::System);
        // 手写配置里的常见写法都认（不认识的回落 System，绝不报错）
        for (tag, want) in [
            ("zh", Language::ZhCn),
            ("zh-Hans", Language::ZhCn),
            ("ZH-cn", Language::ZhCn),
            ("en-US", Language::En),
            ("EN", Language::En),
            ("de", Language::System),
            ("", Language::System),
            ("system", Language::System),
        ] {
            assert_eq!(language_from_tag(tag), want, "{tag:?} 应换算成 {want:?}");
        }
        // 行号往返（界面 ComboBox 传回来的是行号）
        for l in [Language::System, Language::ZhCn, Language::En] {
            assert_eq!(Language::from_index(l.to_index()), l);
        }
        assert_eq!(Language::from_index(99), Language::System);
        assert_eq!(Language::from_index(-1), Language::System);
        // 「恢复默认设置」不动它
        let cfg = Config {
            language: Language::En,
            ..Config::default()
        };
        assert_eq!(cfg.restored(Baseline::AppDefaults).language, Language::En);
        assert_eq!(cfg.restored(Baseline::DufsNative).language, Language::En);
    }

    /// 明暗主题的三件事：缺字段/非法值回落、行号往返、**不受「恢复默认设置」影响**。
    ///
    /// ★ 行号是 0=跟随系统 1=浅色 2=深色 —— 和 `Language` 一样是 0/1/2，
    ///   最容易在回调里把两个枚举搞混，所以往返测试必须覆盖。
    #[test]
    fn 明暗主题的行号与回落() {
        // 老配置里没有这个字段 → 不能因此解析失败（否则整份配置丢回默认值）
        let cfg: Config = serde_json::from_str(r#"{"port":5000}"#).expect("缺字段也要能读");
        assert_eq!(cfg.theme_mode, ThemeMode::System);
        // 非法值同样回落（不弹窗、不报错）
        let cfg: Config = serde_json::from_str(r#"{"theme_mode":"sepia"}"#).expect("非法值回落");
        assert_eq!(cfg.theme_mode, ThemeMode::System);
        // 常见写法都认（大小写不敏感；不认识的回落 System，绝不报错）
        for (tag, want) in [
            ("light", ThemeMode::Light),
            ("Dark", ThemeMode::Dark),
            (" DARK ", ThemeMode::Dark),
            ("system", ThemeMode::System),
            ("", ThemeMode::System),
            ("sepia", ThemeMode::System),
        ] {
            assert_eq!(theme_mode_from_tag(tag), want, "{tag:?} 应换算成 {want:?}");
        }
        // 行号往返（界面 ComboBox 传回来的是行号）
        for t in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            assert_eq!(ThemeMode::from_index(t.to_index()), t);
        }
        assert_eq!(ThemeMode::from_index(99), ThemeMode::System);
        assert_eq!(ThemeMode::from_index(-1), ThemeMode::System);
        // 线格式
        assert_eq!(serde_json::to_string(&ThemeMode::Dark).unwrap(), "\"dark\"");
        // 「恢复默认设置」不动它
        let cfg = Config {
            theme_mode: ThemeMode::Dark,
            ..Config::default()
        };
        assert_eq!(cfg.restored(Baseline::AppDefaults).theme_mode, ThemeMode::Dark);
        assert_eq!(cfg.restored(Baseline::DufsNative).theme_mode, ThemeMode::Dark);
    }

    /// 非法值必须在**加载阶段**就被拒 —— 不能靠 `validate_accounts` 兜底
    /// （那条路已经删了：枚举里根本构造不出非法值）。
    #[test]
    fn 非法枚举值被拒() {
        for bad in [r#"{"perm":"xx"}"#, r#"{"perm":"RW"}"#, r#"{"perm":"r"}"#] {
            assert!(
                serde_json::from_str::<AccountRow>(bad).is_err(),
                "{bad} 应当被拒（线格式是小写的 rw/ro）"
            );
        }
        assert!(serde_json::from_str::<Config>(r#"{"compress":"hw"}"#).is_err());
        assert!(serde_json::from_str::<Config>(r#"{"render_mode":"nope"}"#).is_err());
    }

    /// 界面 ComboBox 行号 ⇄ 枚举：往返必须稳定，越界必须收到**只读**。
    #[test]
    fn 界面行号往返与越界回退() {
        for p in [Perm::Rw, Perm::Ro] {
            assert_eq!(Perm::from_index(p.to_index()), p);
        }
        for c in [
            Compress::None,
            Compress::Low,
            Compress::Medium,
            Compress::High,
        ] {
            assert_eq!(Compress::from_index(c.to_index()), c);
        }
        for m in [
            RenderMode::Default,
            RenderMode::Index,
            RenderMode::TryIndex,
            RenderMode::Spa,
        ] {
            assert_eq!(RenderMode::from_index(m.to_index()), m);
        }
        // ★ 越界一律收到只读：不能因为界面传了个怪值就把账号变成可写
        for bad in [-1, 2, 99] {
            assert_eq!(Perm::from_index(bad), Perm::Ro, "越界 {bad} 应当回落只读");
        }
        // 缺 perm 字段 → 也是只读（fail-closed，见 `impl Default for Perm`）
        let row: AccountRow = serde_json::from_str(r#"{"name":"a"}"#).unwrap();
        assert_eq!(row.perm, Perm::Ro);
    }
}
