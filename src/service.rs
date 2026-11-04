//! 服务编排层：把「启动 / 停止 / 换网卡 / 退出」这些动作和 Slint 界面接起来。
//!
//! 线程约定：
//!   - 所有会阻塞的操作（释放 dufs、等待就绪、端口探测、UAC 提权、文件对话框）都在子线程里做；
//!   - 所有对界面属性的写入统一走 `ui::ui_set()` / `slint::invoke_from_event_loop`，
//!     因此本模块的任何函数都可以从任意线程调用。
//!
//! ★ Slint 的属性只能由 UI 线程写。子线程里直接 `ui.set_xxx()` 不会立刻触发重绘，
//!   表现就是"改完要等一会儿才生效"。

use crate::config::{Compress, Config, Perm, RenderMode};
use crate::dufs;
use crate::i18n;
use crate::i18n::K;
use crate::net::{self, Candidate};
use crate::tray;
use crate::ui;
use crate::MainWindow;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// 注意：界面上「所有网卡（推荐）」那个勾选框的文案**在 ui/pages/share.slint 里**
// （它不再经过 Rust，所以这边没有对应常量 —— 以前那个 ALL_NICS_LABEL 已经删掉）。

// ★ 原来这里有 `COMPRESS_LEVELS` / `RENDER_MODES` 两个字符串表和 `level_of` /
//   `index_of` 两个换算函数。现在「行号 ⇄ 语义」的换算**只在枚举里**
//   （`config::Compress::from_index` / `to_index` 等），目标是唯一来源：
//   模板字符串表不会再和配置文件里的字面量各说各话。

/// dufs 子进程 + 保证它不会变成孤儿的 Job 句柄。
///
/// 两者必须放在一起：`job` 一旦 drop，Windows 就会杀掉 job 里的所有进程。
/// 所以正常停止时要先把 `job` 一起清掉，顺序不能反。
struct Runtime {
    child: Option<Child>,
    job: Option<dufs::JobHandle>,
}

/// 全局共享状态（跨线程）
pub struct App {
    rt: Mutex<Runtime>,
    cfg: Mutex<Config>,
    /// 已探测到的候选网卡，索引与「监听网卡」下拉框一致（下拉框第 0 项是"所有网卡"）
    ips: Mutex<Vec<Candidate>>,
    /// 用户已经执行过「清除程序数据」。置位后 `save()` 变成空操作 ——
    /// ★ 否则退出时的 `app.save()` 会把刚删掉的 config.json 又写回来。
    purged: AtomicBool,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        Self {
            rt: Mutex::new(Runtime { child: None, job: None }),
            cfg: Mutex::new(cfg),
            ips: Mutex::new(Vec::new()),
            purged: AtomicBool::new(false),
        }
    }

    /// 标记「数据已清除」，之后不再写配置文件
    pub fn mark_purged(&self) {
        self.purged.store(true, Ordering::SeqCst);
    }

    pub fn cfg(&self) -> Config {
        self.cfg.lock().expect("cfg lock poisoned").clone()
    }

    pub fn is_running(&self) -> bool {
        self.rt.lock().expect("rt lock poisoned").child.is_some()
    }

    /// 整体替换配置（「恢复默认设置」用：先用纯函数算出新的一份，再一次性换掉）
    pub fn replace_cfg(&self, cfg: Config) {
        *self.cfg.lock().expect("cfg lock poisoned") = cfg;
    }

    /// 记住「详细日志」开关
    pub fn set_verbose_log(&self, on: bool) {
        self.cfg.lock().expect("cfg lock poisoned").verbose_log = on;
    }

    /// 记住「开机自启 / 启动后自动共享 / 静默进托盘」三个勾
    /// （注册表里到底是开是关、带不带 --tray，由 autostart 模块负责）
    pub fn set_autostart(&self, on: bool, auto_serve: bool, start_in_tray: bool) {
        let mut cfg = self.cfg.lock().expect("cfg lock poisoned");
        cfg.autostart = on;
        cfg.autostart_serve = auto_serve;
        cfg.start_in_tray = start_in_tray;
    }

    /// 记住「已经提醒过没开认证的自动共享风险」
    pub fn set_warned_autostart_no_auth(&self) {
        self.cfg
            .lock()
            .expect("cfg lock poisoned")
            .warned_autostart_no_auth = true;
    }

    /// 记住用户新选的共享路径，并把它插到「最近使用」的最前面
    pub fn set_serve_path(&self, path: String) {
        let mut cfg = self.cfg.lock().expect("cfg lock poisoned");
        crate::config::remember_recent_path(&mut cfg.recent_paths, &path);
        cfg.serve_path = path;
    }

    /// 从界面属性收集当前配置（用户可能刚改过）。
    ///
    /// ★ 返回 `Err` 时**配置一点都不会被改**（端口校验放在最前面），
    ///   这样调用方可以放心地报错退出，不会把配置写成半成品。
    pub fn sync_cfg_from_ui(&self, ui: &MainWindow) -> Result<(), String> {
        // 先校验端口。解析失败不能静默沿用旧端口 —— 否则会出现
        //「框里写着 80.5、服务实际起在上一个端口」，用户完全看不出来。
        let port_text = ui.get_port_text().trim().to_string();
        if port_text.is_empty() {
            return Err(i18n::t(K::VldPortEmpty).to_string());
        }
        let port: u16 = port_text
            .parse()
            .map_err(|_| i18n::tn(K::VldPortNotInt, &[&format!("{port_text:?}")]))?;
        if port == 0 {
            return Err(i18n::t(K::VldPortZero).to_string());
        }

        let mut cfg = self.cfg.lock().expect("cfg lock poisoned");

        // 服务入口
        cfg.serve_path = ui.get_serve_path().to_string();
        cfg.port = port;

        // 权限与访问控制
        cfg.allow_upload = ui.get_allow_upload();
        cfg.allow_search = ui.get_allow_search();
        cfg.allow_delete = ui.get_allow_delete();
        cfg.readonly_mode = ui.get_readonly_mode();
        cfg.allow_archive = ui.get_allow_archive();
        cfg.compress = Compress::from_index(ui.get_compress_index());
        cfg.allow_hash = ui.get_allow_hash();
        cfg.allow_symlink = ui.get_allow_symlink();
        cfg.enable_cors = ui.get_enable_cors();
        cfg.allow_all = ui.get_allow_all();
        cfg.hidden = ui.get_hidden().to_string();

        // 认证
        cfg.use_auth = ui.get_use_auth();
        // ★ 账号列表不在这里同步：它走下面的 `account-edited` 等回调，
        //   每敲一个字就直接进 Rust。界面上的 model 是**单向**的
        //   （重建会把光标弄丢，所以平时根本不重建），读它只会拿到旧值。
        //   老 username/password 字段已私有化，只做迁移源。
        cfg.path_prefix = ui.get_path_prefix().to_string();

        // 呈现与外部集成
        cfg.render_mode = RenderMode::from_index(ui.get_render_mode_index());
        cfg.tls_cert = ui.get_tls_cert().to_string();
        cfg.tls_key = ui.get_tls_key().to_string();
        cfg.log_file = ui.get_log_file().to_string();
        cfg.log_format = ui.get_log_format().to_string();
        cfg.log_disabled = ui.get_log_disabled();
        cfg.assets = ui.get_assets().to_string();
        cfg.dufs_config = ui.get_dufs_config().to_string();
        cfg.extra_args = ui.get_extra_args().to_string();
        cfg.close_action = crate::config::CloseAction::from_index(ui.get_close_action_index());
        cfg.confirm_on_close = ui.get_confirm_on_close();
        cfg.autostart = ui.get_autostart();
        cfg.autostart_serve = ui.get_autostart_serve();
        cfg.start_in_tray = ui.get_start_in_tray();
        cfg.stay_on_top = ui.get_stay_on_top();
        cfg.language = crate::config::Language::from_index(ui.get_language_index());
        cfg.verbose_log = ui.get_verbose_log();

        Ok(())
    }

    /// 记住关窗默认动作 + 是否询问（下拉框/复选框拨动时调，立即存盘）。
    ///
    /// 单独 setter 而不是走 `sync_cfg_from_ui`：关窗回调里只需要这两个值，
    /// 而且 `sync_cfg_from_ui` 会校验端口（填错时返回 Err），
    /// 在“关窗退出”这个时机不该因为端口格式而卡住。
    pub fn set_close_prefs(&self, action: crate::config::CloseAction, confirm: bool) {
        let mut cfg = self.cfg.lock().expect("cfg lock poisoned");
        cfg.close_action = action;
        cfg.confirm_on_close = confirm;
    }

    /// 记住「窗口置顶」的选择（复选框拨动时调，立即存盘）。
    pub fn set_stay_on_top(&self, value: bool) {
        self.cfg.lock().expect("cfg lock poisoned").stay_on_top = value;
    }

    /// 改界面语言。
    ///
    /// ★ 只存值；**真的去切语言是 `apply_language()` 的事**（它还要碰界面和托盘）。
    ///   分开是因为「存」是数据层、「切」是界面层，而且启动时的顺序是
    ///   「先按配置切好语言 → 再把配置灌进界面」，不能反。
    pub fn set_language(&self, value: crate::config::Language) {
        self.cfg.lock().expect("cfg lock poisoned").language = value;
    }

    /// 改明暗主题。
    ///
    /// ★ 和 `set_language` 一样只存值 —— **真的去换配色是 Slint 的事**：
    ///   `ui/app.slint` 监听 `theme-index` 变化，把 `Palette.color-scheme` 写成
    ///   `Rules.scheme-of(行号)`，内置控件和我们自己的 token 一起变。
    pub fn set_theme_mode(&self, value: crate::config::ThemeMode) {
        self.cfg.lock().expect("cfg lock poisoned").theme_mode = value;
    }

    /// 单独改某个字符串字段（供「选择证书 / 私钥文件」这类回调使用）
    ///
    /// 支持：`serve_path` | `tls_cert` | `tls_key` | `dufs_config` | `assets`
    pub fn set_text_field(&self, field: &str, value: String) {
        let mut cfg = self.cfg.lock().expect("cfg lock poisoned");
        match field {
            "serve_path" => cfg.serve_path = value,
            "tls_cert" => cfg.tls_cert = value,
            "tls_key" => cfg.tls_key = value,
            "dufs_config" => cfg.dufs_config = value,
            "assets" => cfg.assets = value,
            other => flog!("set_text_field: 未知字段 {other}"),
        }
    }

    /// 把配置灌回界面（启动时用）
    pub fn push_cfg_to_ui(&self, ui: &MainWindow) {
        let cfg = self.cfg.lock().expect("cfg lock poisoned");

        ui.set_serve_path(cfg.serve_path.clone().into());
        // ★ 这两个必须在启动时一起算好，否则从旧配置恢复进来时
        //   界面不知道当前是文件还是文件夹，置灰逻辑就错了。
        let kind = crate::config::serve_path_kind(&cfg.serve_path);
        ui.set_path_valid(kind != crate::config::ServePathKind::Invalid);
        ui.set_path_is_file(kind == crate::config::ServePathKind::File);
        // ★ 「实际共享到哪里」也要在启动时算好：从旧配置恢复进来的路径可能是相对路径，
        //   或者当初选的是符号链接。不补这一行的话，用户不碰输入框就永远看不到它。
        ui.set_path_real_hint(
            crate::config::serve_path_hint(&cfg.serve_path)
                .unwrap_or_default()
                .into(),
        );
        ui.set_port_text(cfg.port.to_string().into());
        ui.set_allow_upload(cfg.allow_upload);
        ui.set_allow_search(cfg.allow_search);
        ui.set_allow_delete(cfg.allow_delete);
        ui.set_readonly_mode(cfg.readonly_mode);
        ui.set_allow_archive(cfg.allow_archive);
        ui.set_compress_index(cfg.compress.to_index());
        ui.set_allow_hash(cfg.allow_hash);
        ui.set_allow_symlink(cfg.allow_symlink);
        ui.set_enable_cors(cfg.enable_cors);
        ui.set_allow_all(cfg.allow_all);
        ui.set_hidden(cfg.hidden.clone().into());
        ui.set_use_auth(cfg.use_auth);
        ui.set_accounts(ModelRc::new(VecModel::from(accounts_entries(&cfg))));
        ui.set_path_prefix(cfg.path_prefix.clone().into());
        ui.set_render_mode_index(cfg.render_mode.to_index());
        ui.set_tls_cert(cfg.tls_cert.clone().into());
        ui.set_tls_key(cfg.tls_key.clone().into());
        ui.set_log_file(cfg.log_file.clone().into());
        ui.set_log_format(cfg.log_format.clone().into());
        ui.set_log_disabled(cfg.log_disabled);
        ui.set_assets(cfg.assets.clone().into());
        ui.set_dufs_config(cfg.dufs_config.clone().into());
        ui.set_extra_args(cfg.extra_args.clone().into());
        ui.set_close_action_index(cfg.close_action.to_index());
        ui.set_confirm_on_close(cfg.confirm_on_close);
        ui.set_autostart(cfg.autostart);
        ui.set_autostart_serve(cfg.autostart_serve);
        ui.set_start_in_tray(cfg.start_in_tray);
        ui.set_recent_paths(ModelRc::new(VecModel::from(recent_entries(&cfg))));
        ui.set_stay_on_top(cfg.stay_on_top);
        ui.set_verbose_log(cfg.verbose_log);
        ui.set_language_index(cfg.language.to_index());
        ui.set_theme_index(cfg.theme_mode.to_index());
    }

    pub fn save(&self) {
        // 清除数据之后不准再写回去（否则退出时会把刚删的配置复活）
        if self.purged.load(Ordering::SeqCst) {
            return;
        }
        self.cfg.lock().expect("cfg lock poisoned").save();
    }
}

// 窗口置顶
// ============================================================

/// `set_topmost` 的失败原因。
///
/// ★ 结果会**直接显示在界面提示里**（`callbacks.rs` 把它 `set_hint` 出去），
///   所以要和文案分开，让断言/判断不看中文（见 `config::AccountProblem`）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TopmostError {
    /// 按 PID 也找不到主窗口（几乎只会在窗口还没建好时发生）
    WindowNotFound,
    /// `SetWindowPos` 本身失败
    Os(String),
    /// 非 Windows 平台。★ 只在非 Windows 构建里会被构造（那边的分支是 cfg 掉的），
    /// 所以在 Windows 上它是"死变体" —— 精确地在这里放行，免得每个平台各留一份枚举。
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
}

impl TopmostError {
    /// 给人看的文案。★ **显示层** —— Step 5 起把这些字面量搬进 i18n catalog。
    pub fn describe(&self) -> String {
        match self {
            TopmostError::WindowNotFound => i18n::t(K::TopmostWindowNotFound).to_string(),
            TopmostError::Os(e) => i18n::tn(K::TopmostOs, &[e]),
            TopmostError::Unsupported => i18n::t(K::TopmostUnsupported).to_string(),
        }
    }
}

impl std::fmt::Display for TopmostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// 窗口置顶 / 取消置顶（Windows）。
///
/// 用 `SetWindowPos` 切换 `WS_EX_TOPMOST`。`SWP_NOACTIVATE` 保证切换时不抢焦点
/// （否则勾选瞬间焦点乱跳）；`SWP_NOMOVE | SWP_NOSIZE` 保证位置大小不动。
/// 窗口句柄靠 **PID** 找（`window_finder::main_hwnd()`），和拖拽注册 / 托盘
/// 是同一套机制。★ 不再按标题找 —— 标题是要翻译的文案。
///
/// SetWindowPos 本身很快（一次系统调用），直接在 UI 线程调就行，不用丢后台。
#[cfg(windows)]
pub fn set_topmost(top: bool) -> Result<(), TopmostError> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    let raw = crate::window_finder::main_hwnd().ok_or(TopmostError::WindowNotFound)?;
    let z = if top { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        SetWindowPos(
            HWND(raw as *mut _),
            Some(z),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .map_err(|e| TopmostError::Os(e.to_string()))?;
    }
    Ok(())
}

/// 非 Windows 占位：保持接线代码不用写两套。
#[cfg(not(windows))]
pub fn set_topmost(_top: bool) -> Result<(), TopmostError> {
    Err(TopmostError::Unsupported)
}

/// 把存好的置顶设置应用到窗口（启动时用）。失败只记日志，不打断启动。
pub fn apply_saved_topmost(app: &Arc<App>) {
    if app.cfg().stay_on_top {
        match set_topmost(true) {
            Ok(()) => vlog!("已恢复窗口置顶"),
            Err(e) => flog!("恢复窗口置顶失败: {e}"),
        }
    }
}

// ============================================================
// 窗口状态（位置 / 宽度 / 是否最大化）
// ============================================================

/// 宽度用「上次记住的」；没记住就用默认 —— 切运行状态时的窗口尺寸都拿它当基准。
fn preferred_width(app: &Arc<App>) -> f32 {
    app.cfg().window_width.unwrap_or(ui::WINDOW_WIDTH)
}

/// 高度基准（"服务未运行"时的高度）；没记住就返回 None，由 `fit_window` 用默认值。
fn preferred_height(app: &Arc<App>) -> Option<f32> {
    app.cfg().window_height
}

/// 启动时恢复上次的窗口位置、宽度、高度与最大化状态。
///
/// ★ 必须在 `ui.show()` **之前**调：窗口还没创建时 Slint 会把
///   `set_size` / `set_position` / `set_maximized` 记成待应用属性，
///   创建出来就是最终状态 —— 不会出现"先出现在默认位置再跳过去"。
///
/// 高度恢复的是**基准值**（未运行高度），运行时仍会额外加 `QR_EXTRA_HEIGHT`
/// —— 既记得住用户拖出来的高度，也保留了"开服务就变高"。
pub fn restore_window_state(app: &Arc<App>, ui: &MainWindow) {
    let cfg = app.cfg();

    // 高度按"未运行"算（启动时服务必然没跑）
    ui::fit_window(
        ui,
        false,
        cfg.window_width.unwrap_or(ui::WINDOW_WIDTH),
        cfg.window_height,
    );

    let window = ui.window();

    // 位置：可能上次是在一块已经拔掉的显示器上，那就别用，交给 Windows 自己放
    if let (Some(x), Some(y)) = (cfg.window_x, cfg.window_y) {
        if point_on_any_monitor(x, y) {
            window.set_position(slint::PhysicalPosition::new(x, y));
            vlog!("恢复窗口位置: {x},{y}");
        } else {
            flog!("上次的窗口位置 {x},{y} 不在任何显示器上（拔了副屏？），忽略");
        }
    }

    // 最大化放最后：先有正常尺寸，用户取消最大化时才有合理的尺寸可还原
    if cfg.window_maximized {
        window.set_maximized(true);
        vlog!("恢复窗口最大化状态");
    }
}

/// 退出前把窗口位置 / 宽度 / 高度基准 / 最大化状态存回配置。
///
/// 只在 UI 线程调（`quit` 就是在 UI 线程跑的）。
/// 最大化时**不覆盖**位置与尺寸：它们存的是"取消最大化后该回到哪"，
/// 而最大化状态下的 `position()/size()` 是屏幕尺寸，不是用户拖出来的那个。
///
/// ★ 高度要换算成**基准值**再存：如果退出时服务还在跑，当前高度里含了
///   二维码那一块（`QR_EXTRA_HEIGHT`），换算靠 `ui::base_height_from` ——
///   和 `fit_window` 用的是同一个函数，两边口径不可能对不上。
pub fn remember_window_state(app: &Arc<App>, ui: &MainWindow) {
    let window = ui.window();
    let maximized = window.is_maximized();
    let minimized = window.is_minimized();
    let running = app.is_running();

    let mut cfg = app.cfg();
    cfg.window_maximized = maximized;

    if !maximized && !minimized {
        let pos = window.position();
        let size = window.size();
        let scale = window.scale_factor();
        // 窗口已经销毁时拿到的可能是 0，存下去下次就成"空白窗口"了
        if size.width > 0 && size.height > 0 {
            cfg.window_x = Some(pos.x);
            cfg.window_y = Some(pos.y);
            cfg.window_width = Some(size.width as f32 / scale);
            cfg.window_height = Some(ui::base_height_from(
                size.height as f32 / scale,
                running,
            ));
        }
    }

    app.replace_cfg(cfg.clone());
    // ★ 不在这里存盘：调用方 `quit()` 会统一存一次（见那里）。
    vlog!(
        "已记住窗口状态: pos={:?},{:?} w={:?} h={:?} maximized={}",
        cfg.window_x,
        cfg.window_y,
        cfg.window_width,
        cfg.window_height,
        cfg.window_maximized
    );
}

/// 这个屏幕坐标是否落在某块显示器上。
///
/// 用 `MonitorFromPoint(MONITOR_DEFAULTTONULL)`：
/// 不在任何显示器上时它返回 NULL，比自己算虚拟屏幕范围靠谱（多屏、负坐标都对）。
/// 取不到时**当作在**（宁可按用户存的位置放，也不要把它丢到随机位置）。
#[cfg(windows)]
fn point_on_any_monitor(x: i32, y: i32) -> bool {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL};
    unsafe { !MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL).is_invalid() }
}

#[cfg(not(windows))]
fn point_on_any_monitor(_x: i32, _y: i32) -> bool {
    true
}

// ============================================================
// 共享路径校验
// ============================================================

/// 异步校验共享路径（文件夹或单个文件）是否存在，结果写回界面。
///
/// 写两个属性：
///   - `path-valid`：路径有效（是文件夹或文件）——「启动服务」按钮要用；
///   - `path-is-file`：是单个文件 ——「共享」页据此把无意义的选项置灰
///     （实测单文件模式下 dufs 的上传/删除会返回 200 但什么都不做）。
///
/// 因为路径框可以手输/粘贴，输错时要给即时反馈。
/// 每次校验带一个自增序号，只有最新的那次结果会被采用 ——
/// 否则快速输入时旧结果可能后到达，把新结果的界面覆盖掉。
pub fn validate_serve_path(ui: &Weak<MainWindow>, path: String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static GEN: AtomicU64 = AtomicU64::new(0);

    let my_gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ui = ui.clone();

    std::thread::spawn(move || {
        let kind = crate::config::serve_path_kind(&path);
        let valid = kind != crate::config::ServePathKind::Invalid;
        let is_file = kind == crate::config::ServePathKind::File;
        // 「实际共享到哪里」。相对路径按进程工作目录解析、符号链接会被 dufs
        // canonicalize 成目标 —— 两种情况用户都看不见，所以在界面上显式写出来。
        // （和用户填的是同一处时不显示，见 `config::serve_path_hint`。）
        let real_hint = crate::config::serve_path_hint(&path).unwrap_or_default();

        // 已经有更新的校验发起过了，本次结果丢弃
        if GEN.load(Ordering::SeqCst) != my_gen {
            return;
        }
        ui::ui_set(&ui, move |ui| {
            ui.set_path_valid(valid);
            ui.set_path_is_file(is_file);
            ui.set_path_real_hint(real_hint.into());
        });
    });
}

// ============================================================
// 监听网卡 / 访问地址 / 二维码
// ============================================================

/// 计算"当前该用哪个 IP 生成二维码"。
///
/// 优先级：
///   1. 绑定了具体网卡（`-b <ip>`）→ 只有那个地址能连上，没得选；
///   2. 没绑（所有网卡）→ 用用户选定的显示地址 `preferred_ip`；
///   3. 没选过 / 选的地址不在了 → 取评分最高的那个。
///
/// ★ 第 2 条就是「运行中切换访问地址」的实现：只改显示，不影响 dufs。
fn display_candidate(app: &Arc<App>) -> Option<Candidate> {
    let cfg = app.cfg();
    let (list, index) = address_choices(&cfg.bind_ips, &cfg.preferred_ip, &app.ips.lock().expect("cfg lock poisoned"));
    list.get(index).cloned()
}

/// 算出「访问地址」下拉框该列哪些地址、当前选的是第几个。
///
/// 纯函数（不碰全局状态），方便直接拿合成数据写断言。
///
/// 关键是**可达性**：
///   - 勾了具体地址 → 只有勾上的那些地址可达，列出来的必须全是它们
///     （否则用户切到一个没绑的地址，扫出来是个死二维码）；
///   - 一项都没勾（= 所有网卡）→ 全部候选都可达，按 `preferred_ip` 挑一个显示。
pub fn address_choices(
    bind_ips: &[String],
    preferred_ip: &str,
    candidates: &[Candidate],
) -> (Vec<Candidate>, usize) {
    let binds: Vec<&str> = bind_ips
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // 情况一：勾了具体地址 —— 只有它们可达，按用户勾选的顺序列出来
    if !binds.is_empty() {
        let list: Vec<Candidate> = binds
            .iter()
            .map(|bind| {
                candidates
                    .iter()
                    .find(|c| c.ip.to_string() == **bind)
                    .cloned()
                    .unwrap_or_else(|| Candidate {
                        // 勾的地址不在候选里（手改过配置 / 网卡正在重启）
                        // 也要显示出来，否则用户看不到自己绑的是什么
                        ip: bind.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                        name: i18n::t(K::IfaceSpecified).into(),
                        score: 50, // 不要低到 50 以下，否则会被当成虚拟网卡弹黄字
                    })
            })
            .collect();
        // 绑了多个地址时，用户可以在这几个之间切显示（它们都是可达的）
        let index = list
            .iter()
            .position(|c| c.ip.to_string() == preferred_ip.trim())
            .unwrap_or(0);
        return (list, index);
    }

    // 情况二：所有网卡 —— 候选地址都可达，按用户的选择挑一个显示
    let index = candidates
        .iter()
        .position(|c| c.ip.to_string() == preferred_ip.trim())
        .unwrap_or(0);
    (candidates.to_vec(), index)
}

/// 把「访问地址」下拉框的选项和当前选中项写进界面。
///
/// ★ 只能从 UI 线程调用（放在 `ui_set` 的闭包里）。
fn apply_addr_choices(app: &Arc<App>, ui: &MainWindow) {
    let cfg = app.cfg();
    let (list, index) = address_choices(&cfg.bind_ips, &cfg.preferred_ip, &app.ips.lock().expect("cfg lock poisoned"));
    let items: Vec<SharedString> = list.iter().map(|c| c.ip.to_string().into()).collect();
    ui.set_addr_list(ModelRc::new(VecModel::from(items)));
    ui.set_addr_index(index as i32);
}

/// 枚举网卡并刷新「监听网卡」下拉框。用户上次的选择会被优先选中。
///
/// 同时会把**已经不存在**的保存值清掉：换个网络、拔了网线、禁用了网卡之后，
/// 配置里那个 IP 就失效了。此时下拉框会回落到「所有网卡」—— 如果不同步清掉配置，
/// 就会变成「界面显示所有网卡、实际却按旧 IP 传 -b」，启动必然失败。
pub fn refresh_ips(app: &Arc<App>, ui: &Weak<MainWindow>, reason: &str) {
    // 网卡列表**只在启动时枚举一次**，之后再没有任何自动刷新，
    // 所以这里记一笔「这次是谁让我刷的」—— 排查"列表里怎么没有刚插上的网卡"时
    // 需要知道到底刷没刷、什么时候刷的。
    vlog!("重新检测网卡列表（{reason}）");
    let candidates = net::candidates();

    // 先把失效的保存值清掉（先在一个块里改完、放下锁再 save，避免死锁）
    //
    // ★ 注意 `!candidates.is_empty()` 这个前提：如果机器当前压根没网
    //   （拔了网线 / 飞行模式），candidates 是空的，但这**不代表**用户
    //   保存的那些网卡已经不存在了 —— 下次插上网线可能还在。
    //   这种情况下不要动配置。
    // ★ 判据统一到 `net::missing_binds`（和"启动服务前再查一遍"用的是同一套）：
    //   原来这里拿 `candidates` 去比，而 candidates 为了挑"手机最可能连上的那个"
    //   特意剔掉了回环 / link-local 等等 —— 于是**合法绑了 127.0.0.1 的用户
    //   会被当成"地址失效"清掉**。这种地址用户确实可以绑（比如 `127.0.0.1`）。
    let stale: Vec<String> = {
        let local = net::local_addresses();
        let mut cfg = app.cfg.lock().expect("cfg lock poisoned");
        let missing = net::missing_binds(&cfg.bind_ips, &local);
        cfg.bind_ips
            .retain(|b| !missing.iter().any(|m| m == b.trim()));
        missing
    };
    if !stale.is_empty() {
        flog!(
            "保存的监听地址 {} 已不存在，已从监听列表里去掉",
            stale.join(", ")
        );
        app.save();
        ui::set_hint(
            ui,
            i18n::tn(K::IfaceStaleRemoved, &[&stale.join(i18n::list_sep())]),
            false,
        );
    }

    *app.ips.lock().expect("cfg lock poisoned") = candidates;
    refresh_bind_ui(app, ui);
    update_endpoint(app, ui);
}

/// 把候选地址按「平铺两列」拆成左右两栏（偶数位进左栏、奇数位进右栏）。
///
/// ★ 抽成纯函数是因为界面上的取下标算法必须和它严格一致：
///   左栏第 `i` 项 = 候选里的第 `2i` 项，右栏第 `j` 项 = 第 `2j+1` 项。
///   一旦错位，用户勾 A 却绑上了 B，而且**不会有任何报错**。
///   所以这条规则在 Rust 侧有断言（见「平铺两栏」那组用例）。
pub fn tile_columns(ips: &[String]) -> (Vec<String>, Vec<String>) {
    let left = ips.iter().step_by(2).cloned().collect();
    let right = ips.iter().skip(1).step_by(2).cloned().collect();
    (left, right)
}

/// 勾选 / 取消勾选一个监听地址之后，新的监听列表应该是什么。
///
/// 抽成纯函数是为了能直接写断言 —— 这套规则有 5 种情况，光靠手点界面很容易漏掉
/// 其中一两种（尤其「取消最后一个具体地址会回到所有网卡」这种）。
///
/// 数据模型只有一条规则：**空数组 = 所有网卡**，其余都是它的推论：
///   - 勾「所有网卡」（index 0）→ 清空（于是其它勾选自动取消）
///   - 在「所有网卡」状态下勾某个地址 → 变成只绑它（从"全部"收窄到"具体"）
///   - 已经有具体地址时再勾一个 → 追加（**这就是多选**）
///   - 取消最后一个具体地址 → 回到「所有网卡」
///   - 取消「所有网卡」→ 不动：那样一个地址都不绑，dufs 没有可绑的地址会起不来
///
/// `ip` 是 `index` 对应的地址；index 0（「所有网卡」）和越界时传 `None`。
pub fn toggle_bind(
    bind_ips: &[String],
    index: usize,
    checked: bool,
    ip: Option<&str>,
) -> Vec<String> {
    let mut list: Vec<String> = bind_ips.to_vec();

    if index == 0 {
        return if checked { Vec::new() } else { list };
    }
    let Some(ip) = ip else {
        return list; // 越界：不动
    };

    if !checked {
        list.retain(|b| b != ip);
        return list; // 空了就等于回到「所有网卡」
    }
    if list.is_empty() {
        return vec![ip.to_string()]; // 从「所有网卡」收窄到这个地址
    }
    if !list.iter().any(|b| b == ip) {
        list.push(ip.to_string());
    }
    list
}

/// 只刷「监听网卡」那一块（地址列表 + 勾选状态），**不重新枚举网卡**。
///
/// 抽出来是因为勾选 / 取消勾选之后也要立刻回显 —— 那时再去 `net::candidates()`
/// 枚举一遍既慢又没必要（还可能正好赶上网卡状态在变）。
fn refresh_bind_ui(app: &Arc<App>, ui: &Weak<MainWindow>) {
    let candidates = app.ips.lock().expect("cfg lock poisoned").clone();
    let bind_ips = app.cfg.lock().expect("cfg lock poisoned").bind_ips.clone();

    let ips: Vec<String> = candidates.iter().map(|c| c.ip.to_string()).collect();
    let checked: Vec<bool> = ips
        .iter()
        .map(|ip| bind_ips.iter().any(|b| b.trim() == ip))
        .collect();
    let (left, right) = tile_columns(&ips);

    // 折叠起来的时候，**也要让用户看得见自己到底绑了什么** ——
    // 否则过一会儿他会忘掉，然后纳闷"为什么手机连不上"。
    let summary = if bind_ips.is_empty() {
        String::new()
    } else {
        i18n::tn(K::IfaceBoundSummary, &[&bind_ips.join(i18n::list_sep())])
    };

    let left: Vec<SharedString> = left.into_iter().map(|s| s.into()).collect();
    let right: Vec<SharedString> = right.into_iter().map(|s| s.into()).collect();
    let all = bind_ips.is_empty();

    ui::ui_set(ui, move |ui| {
        ui.set_all_nics_checked(all);
        ui.set_bind_summary(summary.into());
        ui.set_bind_left(ModelRc::new(VecModel::from(left)));
        ui.set_bind_right(ModelRc::new(VecModel::from(right)));
        ui.set_bind_checked(ModelRc::new(VecModel::from(checked)));
    });
}

/// 落盘 + 刷界面的统一收尾（两个入口共用）。
///
/// `flat_index` 是 `toggle_bind` 的下标口径：**0 = 「所有网卡」**，候选从 1 开始。
fn apply_bind_change(
    app: &Arc<App>,
    ui: &Weak<MainWindow>,
    flat_index: usize,
    checked: bool,
    ip: Option<&str>,
) {
    let new_list = {
        let cfg = app.cfg.lock().expect("cfg lock poisoned");
        toggle_bind(&cfg.bind_ips, flat_index, checked, ip)
    };

    if app.cfg.lock().expect("cfg lock poisoned").bind_ips == new_list {
        // 没有变化（比如取消「所有网卡」），但界面上的勾已经被点乱了
        // —— 用配置里的真值刷回去
        refresh_bind_ui(app, ui);
        return;
    }
    app.cfg.lock().expect("cfg lock poisoned").bind_ips = new_list.clone();
    app.save();
    vlog!(
        "监听地址改为: {}",
        if new_list.is_empty() {
            "所有网卡".to_string()
        } else {
            new_list.join(", ")
        }
    );

    refresh_bind_ui(app, ui);
    update_endpoint(app, ui);
}

/// 用户勾选 / 取消了「所有网卡」。
pub fn change_bind_all(app: &Arc<App>, ui: &Weak<MainWindow>, checked: bool) {
    apply_bind_change(app, ui, 0, checked, None);
}

/// 用户勾选 / 取消了第 `cand_index` 个候选地址（0 起，对应 `app.ips` 的下标）。
pub fn change_bind_checked(app: &Arc<App>, ui: &Weak<MainWindow>, cand_index: i32, checked: bool) {
    let cand_index = cand_index.max(0) as usize;
    let ip = app
        .ips
        .lock()
        .unwrap()
        .get(cand_index)
        .map(|c| c.ip.to_string());
    // +1：`toggle_bind` 的下标口径是 0 = 「所有网卡」
    apply_bind_change(app, ui, cand_index + 1, checked, ip.as_deref());
}

/// 「最近使用」下拉的条目（`SharedString` 才能进 Slint 的 model）
fn recent_entries(cfg: &Config) -> Vec<SharedString> {
    cfg.recent_paths
        .iter()
        .map(|p| SharedString::from(p.as_str()))
        .collect()
}

/// Config.accounts -> 界面 model 的纯转换。
///
/// ★ `perm` 是枚举，而界面 ComboBox 只认行号（`perm-index`），所以这里是**唯一**
///   一次换算（`Perm::to_index`）。行号顺序即 `ui/pages/share.slint` 里那个
///   ComboBox 的 `model` 顺序。
fn accounts_entries(cfg: &Config) -> Vec<crate::AccountEntry> {
    cfg.accounts
        .iter()
        .map(|r| crate::AccountEntry {
            name: r.name.clone().into(),
            secret: r.secret.clone().into(),
            path: r.path.clone().into(),
            perm_index: r.perm.to_index(),
        })
        .collect()
}

/// 重建账号列表的显示。**只在增/删/加载/恢复默认时调**。
///
/// 平时每敲一个字**不**调 —— 重建 VecModel 会把用户正在打字的光标弄丢。
/// 输入走下面的回调直接进 Rust（见 `account_edited`）。
fn refresh_accounts_ui(app: &Arc<App>, ui: &Weak<MainWindow>) {
    let entries = accounts_entries(&app.cfg());
    ui::ui_set(ui, move |ui| {
        ui.set_accounts(ModelRc::new(VecModel::from(entries)));
    });
}

/// 账号列表里改了一个格子。Rust 是唯一真相来源：这里直接写配置并存盘，
/// **不**重建界面 model（用户正在打字，重建会把光标弄丢）。
/// `field`: 0=用户名 1=密码 2=路径。
pub fn account_edited(app: &Arc<App>, index: i32, field: i32, value: SharedString) {
    {
        let mut cfg = app.cfg.lock().expect("cfg lock poisoned");
        let Some(row) = cfg.accounts.get_mut(index.max(0) as usize) else {
            return;
        };
        match field {
            0 => row.name = value.to_string(),
            1 => row.secret = value.to_string(),
            2 => row.path = value.to_string(),
            _ => {}
        }
    }
    // ★ 这里**不存盘**：本函数是"每敲一个字"都调的，存盘会把 config.json
    //   整个重写一遍（serde 序列化 + 写文件），密码框敲 8 个字符就是 8 次磁盘写。
    //   值已经进了内存里的 `cfg`，会在下列时机落盘：
    //     - 点「启动服务」（`on_start_stop` 里 `sync_cfg_from_ui` 后）
    //     - 退出（`service::quit` → `app.save()`）
    //   顺带一个好处：密码不会因为随手敲几个字就落到磁盘上。
    //
    //   新增/删除账号、改读写权限走的是另外几个回调，那是一次点击，
    //   不是高频事件，所以那几处仍然直接存盘。
}

/// 账号列表里换了读写/只读。`perm_index`: 0=读写(rw)，1=只读(ro)。
///
/// ★ 收的是 ComboBox 的**行号**，不是下拉框文案 —— 以前界面传的是字符串，
///   `v == "只读"` 一旦遇到英文界面就静默失效（原 §3-1 的 bug）。
pub fn account_perm_changed(app: &Arc<App>, index: i32, perm_index: i32) {
    {
        let mut cfg = app.cfg.lock().expect("cfg lock poisoned");
        let Some(row) = cfg.accounts.get_mut(index.max(0) as usize) else {
            return;
        };
        row.perm = Perm::from_index(perm_index);
    }
    app.save();
}

/// 删掉一行账号。删完要重建 model（行号全变了，不重建界面和配置就对不上了）。
pub fn account_removed(app: &Arc<App>, ui: &Weak<MainWindow>, index: i32) {
    {
        let mut cfg = app.cfg.lock().expect("cfg lock poisoned");
        let i = index.max(0) as usize;
        if i < cfg.accounts.len() {
            cfg.accounts.remove(i);
        }
    }
    app.save();
    refresh_accounts_ui(app, ui);
}

/// 加一行空白账号。路径默认 `/`、权限默认读写（最常用的样子，开箱就能填）。
pub fn account_added(app: &Arc<App>, ui: &Weak<MainWindow>) {
    {
        let mut cfg = app.cfg.lock().expect("cfg lock poisoned");
        cfg.accounts.push(crate::config::AccountRow {
            name: String::new(),
            secret: String::new(),
            path: "/".to_string(),
            perm: Perm::Rw,
        });
    }
    app.save();
    refresh_accounts_ui(app, ui);
}

/// 当前配置下，手机该访问的完整地址。
///
/// ★ 三个因素必须一起看，少一个用户就会拿到一个打不开的地址：
///   - 哪张网卡（由调用方选定的 ip 决定）
///   - 协议：配了 TLS 就是 https
///   - URL 前缀：配了 `--path-prefix` 就必须带上，
///     否则 dufs 返回 `400 Invalid Path`（二维码扫出来是个错误页）
///
/// 之前这里只拼 `ip:port`，把后两项忘了 —— 所以开了前缀或 HTTPS 的
/// 用户拿到的是错地址。现在统一从这里出，避免两个调用点各拼一份。
fn access_url(cfg: &Config, ip: IpAddr) -> String {
    let https = !cfg.tls_cert.trim().is_empty() && !cfg.tls_key.trim().is_empty();
    net::access_url(ip, cfg.port, https, &cfg.path_prefix)
}

/// 把用户名密码嵌进访问地址：`http://user:pass@10.0.0.5:5000/`。
///
/// 手机浏览器打开这种链接就不用再手输密码。
/// 用户名/密码里可能有 `@ : / ? # [ ]` 这些分隔符，必须百分号编码，
/// 否则链接会被解析器拆错。
pub fn auth_url(cfg: &Config, url: &str) -> Option<String> {
    if !cfg.use_auth {
        return None;
    }
    // 取第一条"既填了用户名又填了密码"的账号
    let row = cfg
        .accounts
        .iter()
        .find(|r| !r.name.trim().is_empty() && !r.secret.trim().is_empty())?;
    let scheme_end = url.find("://")? + 3;
    Some(format!(
        "{}{}:{}@{}",
        &url[..scheme_end],
        percent_encode(&row.name),
        percent_encode(&row.secret),
        &url[scheme_end..]
    ))
}

/// URL userinfo 部分的百分号编码：只放行 unreserved 字符（RFC 3986）。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 用户在「手机访问 → 访问地址」里换了地址。
///
/// ★ 这是**纯显示**切换：只写 `preferred_ip`，不碰 `bind_ips`、
///   也不重启 dufs —— 所以运行中随时可以切。
///   （前提是 `-b` 绑的是"所有网卡"，否则列表里只有一项，见 `address_choices`）
pub fn change_addr(app: &Arc<App>, ui: &Weak<MainWindow>, value: &str) {
    let ip = value.trim();
    if ip.is_empty() || app.cfg.lock().expect("cfg lock poisoned").preferred_ip == ip {
        return;
    }
    app.cfg.lock().expect("cfg lock poisoned").preferred_ip = ip.to_string();
    app.save();
    vlog!("切换访问地址（仅显示）: {ip}");
    update_endpoint(app, ui);
}

/// 根据当前监听设置刷新访问地址和二维码
fn update_endpoint(app: &Arc<App>, ui: &Weak<MainWindow>) {
    let cfg = app.cfg();
    let candidate = display_candidate(app);

    // 不管有没有可用地址，下拉框都得刷新（否则运行中它会一直是空的）
    {
        let app = app.clone();
        ui::ui_set(ui, move |ui| apply_addr_choices(&app, ui));
    }

    let Some(candidate) = candidate else {
        ui::ui_set(ui, |ui| {
            ui.set_url_text(i18n::t(K::UrlNoLanIp).into());
            ui.set_ip_warning("".into());
        });
        return;
    };

    let url = access_url(&cfg, candidate.ip);
    let url_for_qr = url.clone();

    // 绑到虚拟网卡上一定要提醒：否则用户会一直纳闷"为什么手机扫码打不开"
    // 保持单行：换行会把「手机访问」区块撞高，进而把其它内容挤出可视区
    let warning: SharedString = if candidate.likely_virtual() {
        i18n::tn(K::UrlVirtualIface, &[&candidate.name]).into()
    } else {
        "".into()
    };
    let warn_log = candidate.likely_virtual();
    let cand_ip = candidate.ip;
    let cand_name = candidate.name.clone();

    if warn_log {
        flog!(
            "⚠ 监听地址 {}（{}）看起来是虚拟网卡，手机可能连不上",
            cand_ip,
            cand_name
        );
    } else {
        vlog!(
            "访问地址: {url}（bind={}）",
            if cfg.bind_ips.is_empty() {
                "0.0.0.0".to_string()
            } else {
                cfg.bind_ips.join(", ")
            }
        );
    }

    ui::ui_set(ui, move |ui| {
        ui.set_url_text(url.into());
        ui.set_ip_warning(warning);
        // 在 UI 线程现场生成二维码（Image 不是 Send）
        if let Some(qr) = crate::qr::make_image(&url_for_qr) {
            ui.set_qr_image(qr);
        }
    });
}

// ============================================================
// 启动 / 停止
// ============================================================

/// 启动 dufs（在子线程里完成，界面只显示 busy）
pub fn start_service(app: Arc<App>, ui: Weak<MainWindow>) {
    if app.is_running() {
        return;
    }

    ui::set_busy(&ui, true);
    ui::set_hint(&ui, "", false);
    ui::set_status(&ui, i18n::t(i18n::K::StatusStarting));

    std::thread::spawn(move || {
        let mut cfg = app.cfg();
        // 每次启动都重新探测 IPv6：换机器、网卡被禁用、系统里关掉 IPv6 都会变。
        // 因此既不缓存到文件，也不依赖上一次的结果（见 `Config::ipv6_available`）。
        cfg.ipv6_available = net::ipv6_available();

        // 启动前再查一遍保存的监听地址在本机还在不在。
        //
        // 为什么要查：运行中拔网线 / 关掉 VPN / 禁用网卡之后，配置里那个地址就没了。
        // 不查的话 dufs 会甩回一句
        //     Error: Failed to bind `10.99.99.99:5000`
        // 用户完全看不懂（他甚至不知道这个地址是从哪来的）。
        //
        // 处理方式：**去掉失效的那些**（一个都不剩时，"空数组"自然就是「所有网卡」），
        // 把清理结果落盘，然后把这件事写进提示里告诉用户。

        //所以missing就是失效的网址
        let missing = net::missing_binds(&cfg.bind_ips, &net::local_addresses());
        let bind_notice = if missing.is_empty() {
            String::new()
        } else {
            cfg.bind_ips
                .retain(|b| !missing.iter().any(|m| m == b.trim()));
            app.cfg.lock().expect("cfg lock poisoned").bind_ips = cfg.bind_ips.clone();
            app.save();
            let now = if cfg.bind_ips.is_empty() {
                i18n::t(K::IfaceAllBracket).to_string()
            } else {
                format!("「{}」", cfg.bind_ips.join("、"))
            };
            flog!(
                "启动前检查：监听地址 {} 在本机已不存在，已去掉，改用 {now}",
                missing.join(", ")
            );
            i18n::tn(K::IfaceStaleSwitched, &[&missing.join(i18n::list_sep()), &now])
        };

        // 参数不对就别去启动 dufs 了，直接告诉用户哪里不对
        if let Err(e) = cfg.validate() {
            ui::set_hint(&ui, e, true);
            ui::set_status(&ui, i18n::t(i18n::K::StatusStartFailed));
            ui::set_busy(&ui, false);
            return;
        }

        match dufs::launch(&cfg) {
            Ok((child, job)) => {
                {
                    let mut rt = app.rt.lock().expect("cfg lock poisoned");
                    rt.child = Some(child);
                    rt.job = Some(job);
                }

                app.save();
                tray::set_running(true);

                let ip = display_candidate(&app).map(|c| c.ip);
                let url = ip.map(|ip| access_url(&cfg, ip)).unwrap_or_default();

                // ---------- 一次性提示 ----------
                let (first_time_tip, no_auth_tip) = {
                    let mut c = app.cfg.lock().expect("cfg lock poisoned");
                    let first = !c.firewall_prompted;
                    c.firewall_prompted = true;
                    let need_auth_warn = !c.use_auth && !c.warned_no_auth;
                    c.warned_no_auth = true;
                    (first, need_auth_warn)
                };
                let readonly = app.cfg.lock().expect("cfg lock poisoned").readonly_mode;
                // 单文件模式下「上传」是个假动作（dufs 返回 200 但什么都不写），
                // 提示文案不能再说「并上传文件」，否则用户会被误导。
                let single_file = crate::config::serve_path_kind(&cfg.serve_path)
                    == crate::config::ServePathKind::File;
                app.save();

                let hint = if no_auth_tip {
                    {
                        let verb = if single_file {
                            i18n::t(K::HintNoAuthVerbFileOnly)
                        } else if readonly {
                            i18n::t(K::HintNoAuthVerbDownload)
                        } else {
                            i18n::t(K::HintNoAuthVerbUpload)
                        };
                        i18n::tn(K::HintNoAuth, &[&verb])
                    }
                } else if first_time_tip {
                    i18n::t(K::HintFirewallTip).to_string()
                } else {
                    String::new()
                };
                // 监听地址被挪动过的话，补在最前面 —— 它比那两条一次性提示更要紧
                let hint = if bind_notice.is_empty() {
                    hint
                } else {
                    format!("{bind_notice}{hint}")
                };

                // 下拉框需要读 app 里的候选网卡，所以克隆一份给 UI 闭包
                let app_in_ui = app.clone();
                ui::ui_set(&ui, move |ui| {
                    // ★ 先算尺寸再改 running：fit_window 靠 ui.running 判断
                    //   当前高度是哪种口径（见它的注释）
                    ui::fit_window(ui, true, preferred_width(&app_in_ui), preferred_height(&app_in_ui));
                    ui.set_running(true);
                    ui.set_busy(false);
                    ui.set_status_text(
                        i18n::tn(i18n::K::StatusRunning, &[&cfg.port]).into(),
                    );
                    ui.set_url_text(url.clone().into());
                    // 在 UI 线程现场生成二维码（Image 不是 Send）
                    if let Some(qr) = crate::qr::make_image(&url) {
                        ui.set_qr_image(qr);
                    }
                    // 把「访问地址」下拉框填上（运行中就能用它切地址）
                    apply_addr_choices(&app_in_ui, ui);
                    ui.set_hint_is_error(false);
                    ui.set_hint_text(hint.into());
                });
            }
            Err(e) => {
                tray::set_running(false);
                ui::ui_set(&ui, move |ui| {
                    ui.set_running(false);
                    ui.set_busy(false);
                    ui.set_status_text(i18n::t(i18n::K::StatusStartFailed).into());
                });
                ui::set_hint(&ui, e, true);
            }
        }
    });
}

/// 停止 dufs。kill + wait 都很快，直接在调用线程执行即可。
/// 只负责把 dufs 干掉 + 关 job 句柄。**不碰界面** —— 给退出路径复用。
fn stop_dufs(app: &Arc<App>) {
    let mut rt = app.rt.lock().expect("cfg lock poisoned");
    if let Some(mut child) = rt.child.take() {
        let pid = child.id();
        dufs::stop(&mut child);
        flog!("dufs pid={pid} 已终止");
    }
    // drop job：句柄关闭 -> 内核连带清理 job 里残留的进程
    rt.job = None;
}

/// 退出路径专用的「静默停止」：停 dufs + 通知托盘，但**不刷界面**。
///
/// ★ 为什么要有它：`quit()` 里窗口已经在 `hide()` 之后，再跑 `fit_window` /
///   `set_running` / `set_qr_image` 既没意义，还可能在退出前触发一次多余的重绘。
fn stop_service_silent(app: &Arc<App>) {
    flog!("停止服务（退出路径，不刷界面）");
    stop_dufs(app);
    tray::set_running(false);
}

pub fn stop_service(app: &Arc<App>, ui: &Weak<MainWindow>) {
    flog!("停止服务");
    stop_dufs(app);

    tray::set_running(false);

    let width = preferred_width(app);
    let base_h = preferred_height(app);
    ui::ui_set(ui, move |ui| {
        // ★ 同上：先算尺寸再改 running
        ui::fit_window(ui, false, width, base_h);
        ui.set_running(false);
        ui.set_busy(false);
        ui.set_status_text(i18n::t(i18n::K::StatusStopped).into());
        ui.set_qr_image(slint::Image::default());
    });
}

/// 完整退出：停服务 -> 存配置 -> 收托盘 -> 退出事件循环
pub fn quit(app: &Arc<App>, ui: &Weak<MainWindow>) {
    flog!("收到退出请求");
    // ★ 先装看门狗：万一下面任何一步卡住（尤其是进程退出阶段），
    //   3 秒后硬杀，绝不留一个僵尸进程占着前台把别的程序卡死。
    crate::arm_exit_watchdog();
    // ★ 先记窗口状态：后面会 stop / shutdown，窗口可能很快就没了
    if let Some(ui) = ui.upgrade() {
        remember_window_state(app, &ui);
        // ★ 再隐藏窗口：后面的收尾可能阻塞（`tray::shutdown` 等 Explorer 摘图标
        //   最长 3 秒、dufs 的 wait 也可能慢），藏起来这些等待用户就看不见了。
        let _ = ui.window().hide();
    }
    // 退出路径用静默版：界面马上就要消失了，不必再刷新它
    stop_service_silent(app);
    app.save();
    tray::shutdown();
    let _ = slint::quit_event_loop();
}

// ============================================================
// 高级：打开日志 / 文件选择
// ============================================================

// ============================================================
// 防火墙规则：查看 / 清理
// ============================================================

/// 在后台枚举 `dufs-gui-*` 防火墙规则，结果写回界面。
///
/// 必须后台：实测 `netsh advfirewall firewall show rule name=all` 要 0.4~0.7 秒。
pub fn refresh_firewall_rules(ui: &Weak<MainWindow>) {
    let ui = ui.clone();
    std::thread::spawn(move || {
        let rules = crate::firewall::list_rules();
        let text = if rules.is_empty() {
            i18n::t(K::FwRulesNone).to_string()
        } else {
            let ports: Vec<String> = rules
                .iter()
                .filter_map(|r| r.strip_prefix("dufs-gui-").map(|s| s.to_string()))
                .collect();
            let ports = ports.join(i18n::list_sep());
            let mut t = i18n::pl(
                K::FwRulesOne,
                K::FwRulesMany,
                rules.len(),
                &[&ports],
                &[&ports, &rules.len()],
            );
            if rules.len() > 1 {
                t.push_str(i18n::t(K::FwRulesManyExtra));
            }
            t
        };
        ui::ui_set(&ui, move |ui| ui.set_fw_rules_text(text.into()));
    });
}

/// 清掉所有 `dufs-gui-*` 规则（会弹 UAC，结果由提权实例自己弹框）。
pub fn cleanup_firewall_rules(ui: &Weak<MainWindow>) {
    let ui = ui.clone();
    ui::set_hint(&ui, i18n::t(K::FwCleaningRequest), false);
    std::thread::spawn(move || {
        if let Err(e) = crate::firewall::cleanup_rules() {
            ui::set_hint(&ui, i18n::tn(K::FwCleanFailed, &[&e]), true);
            return;
        }
        // 提权实例会自己弹结果框；等它跑完再刷新一下列表（拿不到就用「刷新」按钮）
        std::thread::sleep(Duration::from_millis(1500));
        refresh_firewall_rules(&ui);
    });
}

// ============================================================
// 清除程序数据（卸载）
// ============================================================

/// 「清除程序数据 / 卸载」：删掉本程序在用户目录下留下的东西。
///
/// 做四件事（两个确认框，都在后台线程）：
///   1. 停服务（dufs.exe 正跑着就删不掉）；
///   2. 问要不要连防火墙规则一起清（要提权）；
///   3. 删 `%LOCALAPPDATA%\dufs-gui`（释放的 dufs + 运行库，~4.8 MB）
///      和 `%APPDATA%\dufs-gui`（配置）；
///   4. 标记不再写配置，报结果，退出。
///
/// 明确**不碰**：用户的共享文件夹、程序自身的 exe（要用户自己删）。
/// Windows 的习惯是卸载不默认删 `%APPDATA%` 里的设置，所以这里把
/// 要删的东西原原本本列在确认框里，不玩猜谜。
pub fn purge_data(app: &Arc<App>, ui: &Weak<MainWindow>) {
    let bin = dufs::bin_dir(); // %LOCALAPPDATA%\dufs-gui\bin
    let Some(local_root) = bin.parent().map(Path::to_path_buf) else {
        ui::set_hint(ui, i18n::t(K::SysNoDataDir), true);
        return;
    };
    let cfg_root = crate::config::config_dir(); // %APPDATA%\dufs-gui
    let was_running = app.is_running();

    let mut text = String::new();
    if was_running {
        text.push_str(i18n::t(K::PurgeRunningWarn));
    }
    text.push_str(&i18n::tn(
        K::PurgeConfirmText,
        &[&local_root.display(), &cfg_root.join("config.json").display()],
    ));

    if !crate::folder_picker::confirm(i18n::t(K::PurgeConfirmTitle), &text) {
        return;
    }

    // 顺便问一句防火墙规则
    let clean_fw = crate::folder_picker::confirm(
        i18n::t(K::FwPurgeConfirmTitle),
        i18n::t(K::FwPurgeConfirmText),
    );

    // 停服务：释放 dufs.exe 的文件占用
    stop_service(app, ui);

    if clean_fw {
        match crate::firewall::cleanup_rules() {
            Ok(()) => flog!("已请求提权实例清理防火墙规则"),
            Err(e) => flog!("清理防火墙规则失败: {e}"),
        }
    }

    // 删目录
    let mut removed: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for dir in [&local_root, &cfg_root] {
        if !dir.exists() {
            continue;
        }
        match std::fs::remove_dir_all(dir) {
            Ok(()) => {
                removed.push(dir.display().to_string());
                flog!("已删除 {}", dir.display());
            }
            Err(e) => {
                failed.push(format!("{}（{e}）", dir.display()));
                flog!("删除 {} 失败: {e}", dir.display());
            }
        }
    }

    // ★ 标记：之后任何 save() 都不再写文件，免得退出时把配置复活
    app.mark_purged();

    let msg = if failed.is_empty() {
        let files = if removed.is_empty() {
            i18n::t(K::PurgeNoFiles).to_string()
        } else {
            removed.join("\n")
        };
        i18n::tn(
            K::PurgeDone,
            &[&files, &i18n::t(if clean_fw { K::FwPurged } else { K::FwKept })],
        )
    } else {
        let kept = if removed.is_empty() {
            i18n::t(K::ListEmpty).to_string()
        } else {
            removed.join("\n")
        };
        i18n::tn(K::PurgePartialFail, &[&failed.join("\n"), &kept])
    };
    crate::firewall::message_box(&msg, i18n::t(K::PurgeConfirmTitle), !failed.is_empty());

    // 收尾退出：config 已删，界面继续开着也没意义
    tray::shutdown();
    let _ = slint::quit_event_loop();
}

/// 用默认程序打开 dufs 的访问日志。没有配日志文件时给出提示。
pub fn open_access_log(app: &Arc<App>, ui: &Weak<MainWindow>) {
    let path = app.cfg().log_file;

    if path.trim().is_empty() {
        ui::set_hint(
            ui,
            i18n::t(K::LogNotConfigured),
            false,
        );
        return;
    }

    if !std::path::Path::new(&path).exists() {
        ui::set_hint(ui, i18n::tn(K::LogMissing, &[&path]), true);
        return;
    }

    crate::firewall::open_path(&path);
}

// ============================================================
// 本程序自己的信息：日志 / 配置文件 / 版本
// ============================================================

/// 打开本程序自己的日志（`%TEMP%\dufs-gui.log`）。
///
/// ★ 与上面的 `open_access_log` 不是同一个文件:
///   那个是 **dufs** 记的“谁访问了什么”,这个是**本程序**记的“自己干了什么”。
pub fn open_app_log(ui: &Weak<MainWindow>) {
    let path = crate::logfile::log_path();
    if !path.exists() {
        ui::set_hint(ui, i18n::t(K::LogAppEmpty), false);
        return;
    }
    crate::firewall::open_path(&path.to_string_lossy());
}

/// 在资源管理器里打开程序日志所在的目录（日志为空、或想拷贝出去时用）。
pub fn open_log_folder(ui: &Weak<MainWindow>) {
    let dir = crate::logfile::log_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    crate::firewall::open_path(&dir.to_string_lossy());
    ui::set_hint(ui, i18n::tn(K::FileOpenedInExplorer, &[&dir.display()]), false);
}

/// 清空程序日志（日志太长时从头看）。
pub fn clear_app_log(ui: &Weak<MainWindow>) {
    crate::logfile::reset();
    flog!("程序日志已被用户清空");
    ui::set_hint(ui, i18n::t(K::LogAppCleared), false);
}

/// 在资源管理器里打开 dufs 释放目录（`%LOCALAPPDATA%\dufs-gui\bin`）。
///
/// ★ 和「打开配置文件」不是一回事：那里放的是 JSON 设置，
///   这里放的是内嵌释放出来的 dufs.exe 和 vcruntime140.dll。
pub fn open_bin_dir(ui: &Weak<MainWindow>) {
    let dir = dufs::bin_dir();
    crate::firewall::open_path(&dir.to_string_lossy());
    ui::set_hint(ui, i18n::tn(K::FileOpenedInExplorer, &[&dir.display()]), false);
}

/// 打开配置文件（JSON，明文）。
pub fn open_config_file(ui: &Weak<MainWindow>) {
    let path = crate::config::config_path();
    if !path.is_file() {
        ui::set_hint(ui, i18n::t(K::CfgFileMissing), false);
        return;
    }
    crate::firewall::open_path(&path.to_string_lossy());
}

/// 切换界面语言 —— **切语言的唯一入口**。
///
/// 界面文案有两套来源，必须同时切，否则会出现「菜单英文、提示中文」：
///
/// 1. **Slint 侧**（所有 `@tr()` 串）：`select_bundled_translation(tag)`。
///    译文是编译期嵌进 exe 的，切语言不需要重启；而且每处 `@tr` 都注册了
///    「语言脏位」依赖，所以**已经创建出来的窗口会当场重算所有文案** ——
///    不需要重建窗口、也不需要重启程序（实测：创建窗口后切换，再读属性就是新语言）。
///
/// 2. **Rust 侧**（`i18n::t()` 串）：`i18n::set_lang()`。
///    这些串在调用时就已变成 `String` 交给界面了，**不会自己变** ——
///    所以要手动重灌下面这几处「由 Rust 写进去的界面文字」。
///
/// ★ 本函数只能在 UI 线程调。
///
/// ★ `select_bundled_translation` 必须在**首个组件创建之后**调：
///   语言包是随第一个组件的初始化注册进线程局部环境的（错误
///   `NoTranslationsBundled` 的文档里就写着这一点）。
fn switch_texts(ui: &MainWindow) {
    ui.set_window_title(crate::main_window_title().into());
    // 状态栏：只重写「与运行状态有关」的那两种；启动中/启动失败是瞬时状态，
    // 下次刷新会自己用新语言重写。
    let app = current_app();
    match app.as_ref() {
        Some(app) if app.is_running() => {
            let port = app.cfg().port;
            ui.set_status_text(i18n::tn(i18n::K::StatusRunning, &[&port]).into());
        }
        _ => ui.set_status_text(i18n::t(i18n::K::StatusNotStarted).into()),
    }
    // 底部提示：它跟着「关闭行为」的设置走，不是临时提示，所以要重写。
    // 临时提示（复制成功、路径不存在…）直接清掉 —— 它们是上一条操作的反馈，
    // 用旧语言摆在那里比没有更让人困惑。
    if let Some(app) = app {
        let cfg = app.cfg();
        ui.set_hint_text(crate::close_hint(cfg.close_action, cfg.confirm_on_close).into());
    } else {
        // App 还没建好（理论上到不了这里）：宁可不显示提示，也别留着旧语言的那条。
        ui.set_hint_text("".into());
    }
    ui.set_hint_is_error(false);
    // 防火墙规则那段是后台 netsh 枚举后写进去的，重扫一次（它会自己用新语言重写）。
    refresh_firewall_rules(&ui.as_weak());
    // 托盘菜单与提示在**另一个线程**上，只能叫醒它重新设一次文字。
    crate::tray::refresh_text();
}

/// 给 `switch_texts` 用的「当前 App」句柄。
///
/// ★ 为什么要全局变量：`App` 是 `Arc`，而切语言的入口有两个
///   （启动时在 `main` 里、运行中在 `callbacks` 里），后者拿得到 `Arc`，
///   前者在构造过程中。与其把参数在两层里传一遍，不如在 `App::new` 时
///   存一份 `Weak` —— 它只是用来查「服务在不在跑、端口是多少」这两个事实。
static APP_FOR_TEXT: std::sync::OnceLock<std::sync::Weak<App>> = std::sync::OnceLock::new();

/// 记下当前 App（`main.rs` 建好 `Arc<App>` 之后立刻调一次）。
pub(crate) fn remember_app(app: &Arc<App>) {
    let _ = APP_FOR_TEXT.set(Arc::downgrade(app));
}

/// 取回当前 App。进程刚起来那一瞬间可能还没有（`OnceLock` 没设过）。
fn current_app() -> Option<Arc<App>> {
    APP_FOR_TEXT.get().and_then(std::sync::Weak::upgrade)
}

/// 按配置切换界面语言。启动早期（创建窗口之后、写任何文案之前）调一次；
/// 用户在「应用」页改语言时也调它。
/// ★ 失败只记日志、不弹窗：`select_bundled_translation` 到什么情况下会失败？
///   语言名不在语言包里（配置被人手改成 `"language": "de"` 已经由 serde
///   回落到 `System` 了，所以只有「exe 编译时没带语言包」这种开发期错误）。
///   为这种事打断用户没有意义，中文源串照样能用。
pub fn apply_language(ui: &MainWindow, lang: crate::config::Language) {
    let rust_lang = i18n::Lang::from_language(lang);
    i18n::set_lang(rust_lang);
    if let Err(e) = slint::select_bundled_translation(rust_lang.slint_tag()) {
        flog!("选择界面语言失败（{lang:?} -> {rust_lang:?}）: {e}");
    }
    switch_texts(ui);
    vlog!("界面语言已切换为 {rust_lang:?}");
}

/// 应用「明暗主题」：把选择写进内置控件共用的 `Palette.color-scheme`。
///
/// ★ 为什么要 Rust 来写而不是在 .slint 里赋值：`Palette.color-scheme` 在
///   std-widgets 里是 `<=> FluentPalette.color-scheme` 的两向绑定，从组件里
///   直接赋值实测不生效；走官方支持的 `ui.global::<Palette>()` 才能一改到底。
///
/// ★ `System` 写的是 `ColorScheme.Unknown` —— fluent 的 `dark-color-scheme`
///   会在 unknown 时回落到系统，所以「跟随系统」是原生语义，不是我们自己猜的。
///   （我们自己那套 token 怎么从 Palette 反推明暗见 `ui/theme.slint` 的 `dark`。）
///
/// ★ 必须在 `show()` 之前调：窗口还没创建时这个设值会被记成待应用属性，
///   建出来就是最终配色，不会先闪一下浅色。
pub fn apply_theme(ui: &MainWindow, mode: crate::config::ThemeMode) {
    use crate::config::ThemeMode;
    // ★ 类型走 `private_unstable_api::re_exports`：`Palette.color-scheme` 的 setter
    //   签名里就是这个 `ColorScheme`（生成代码用 `sp::ColorScheme` 引用它），
    //   Slint 没有在 crate 根目录公开重导出。这是绑定生成代码自己用的路径。
    use slint::private_unstable_api::re_exports::ColorScheme;
    let scheme = match mode {
        ThemeMode::System => ColorScheme::Unknown,
        ThemeMode::Light => ColorScheme::Light,
        ThemeMode::Dark => ColorScheme::Dark,
    };
    ui.global::<crate::Palette>().set_color_scheme(scheme);
    vlog!("明暗主题已应用: {mode:?}");
}

/// 把「关于」要显示的只读信息灌进界面。启动时调一次即可。
/// 「应用 → 关于」的 5 个只读诊断字段：版本与路径，启动时填一次即可。
///
/// ★ 只能在 UI 线程调（启动时 `main` 里直调）：同步立即生效。
///   后台线程想刷这些请走 `ui_set`，别调它。
pub fn push_about_info(ui: &MainWindow) {
    ui.set_app_version(format!("dufs-gui {}", env!("CARGO_PKG_VERSION")).into());
    ui.set_dufs_version(format!("dufs {}", dufs::DUFS_VERSION).into());
    ui.set_config_path(crate::config::config_path().display().to_string().into());
    ui.set_log_path(crate::logfile::log_path().display().to_string().into());
    ui.set_bin_dir(dufs::bin_dir().display().to_string().into());
}

// ============================================================
// 恢复默认设置
// ============================================================

/// 「恢复默认设置」：改回默认值 → 存盘 → 刷新界面。
///
/// 为什么要二次确认:这个动作会把十几项设置一次改掉(包括认证、TLS、日志),
/// 而且没有撤销。确认框在后台线程里弹(它会阻塞)。
pub fn restore_defaults(app: &Arc<App>, ui: &Weak<MainWindow>, app_baseline: bool) {
    let baseline = if app_baseline {
        crate::config::Baseline::AppDefaults
    } else {
        crate::config::Baseline::DufsNative
    };

    let (title, what) = if app_baseline {
        (K::RestoreAppTitle, K::RestoreAppDesc)
    } else {
        (K::RestoreDufsTitle, K::RestoreDufsDesc)
    };
    let title = i18n::t(title);

    let text = i18n::tn(K::RestoreConfirmText, &[&i18n::t(what)]);

    if !crate::folder_picker::confirm(title, &text) {
        return;
    }

    // 改配置(纯函数算出新的一份,再整体换掉)
    let new_cfg = app.cfg().restored(baseline);
    app.replace_cfg(new_cfg);
    // 端口/网卡变了,启动前的校验要重新跑一遍;这里直接同步界面
    app.save();
    flog!(
        "已恢复默认设置（{}），端口={} 网卡='{}'",
        if app_baseline { "本程序默认" } else { "dufs 原生默认(纯只读)" },
        app.cfg().port,
        app.cfg().bind_ips.join(",")
    );

    // 刷界面:push_cfg_to_ui 把新配置写回所有属性
    let app_ui = app.clone();
    ui::ui_set(ui, move |ui| {
        app_ui.push_cfg_to_ui(ui);
        ui.set_hint_is_error(false);
    });
    ui::set_hint(
        ui,
        i18n::tn(K::RestoreDone, &[&title]),
        false,
    );

    // 端口/网卡变了 → 重新枚举一次网卡,把「监听网卡」下拉框对齐到「所有网卡」
    refresh_ips(app, ui, "恢复默认设置");
}

#[cfg(test)]
mod tests {
    use super::{auth_url, percent_encode};
    use crate::config::{AccountRow, Config, Perm};

    /// userinfo 里的分隔符必须被编码，否则链接会被解析器拆错
    /// （`p@ss:w/rd` 原样拼进去就变成 host=ss、path=w/rd）
    #[test]
    fn 编码_url_userinfo() {
        assert_eq!(percent_encode("abc123-._~"), "abc123-._~");
        assert_eq!(percent_encode("p@ss:w/rd"), "p%40ss%3Aw%2Frd");
        assert_eq!(percent_encode("中文"), "%E4%B8%AD%E6%96%87");
        assert_eq!(percent_encode(""), "");
    }

    #[test]
    fn 带密码链接_只在开了认证且有账号时给出() {
        let mut cfg = Config::default();
        let url = "http://10.0.0.5:5000/sub";

        // 没开认证 → 不给
        cfg.use_auth = false;
        assert_eq!(auth_url(&cfg, url), None);

        // 开了认证但没有账号 → 还是不给
        cfg.use_auth = true;
        assert_eq!(auth_url(&cfg, url), None);

        // 账号填了用户名 + 密码 → 拼进 userinfo
        cfg.accounts = vec![AccountRow {
            name: "a@b".into(),
            secret: "p:w".into(),
            path: "/".into(),
            perm: Perm::Rw,
        }];
        assert_eq!(
            auth_url(&cfg, url).unwrap(),
            "http://a%40b:p%3Aw@10.0.0.5:5000/sub"
        );

        // 只有用户名没密码的行跳过（那行是匿名/半成品，拼出来反而进不去）
        cfg.accounts = vec![AccountRow {
            name: "only".into(),
            secret: String::new(),
            path: "/".into(),
            perm: Perm::Rw,
        }];
        assert_eq!(auth_url(&cfg, url), None);
    }
}
