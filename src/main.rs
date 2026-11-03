//! dufs-gui —— 给 dufs 文件服务器套一个图形界面
//!
//! 启动流程：
//!   1. 若带 `--install-firewall <port>` / `--cleanup-firewall` 参数 -> 走提权分支
//!      （普通权限下改不了防火墙，所以程序用 ShellExecuteW("runas") 重新启动自己），
//!      执行完 netsh 直接退出，不建界面
//!   2. 单实例检查（命名互斥量，进程消失自动释放，崩溃也不会留死锁）
//!   3. 选择渲染后端（winit + software，体积小、兼容性好；配置要先读，见下）
//!   4. 建窗口、接线回调、起托盘线程
//!      - 带 `--tray` 时窗口创建完立即隐藏（"静默启动"）——这个参数只由
//!        「登录后自动运行」那条自启项带上，手动双击 exe 永远会显示窗口
//!   5. 进入事件循环

// 发布版不要控制台窗口（提权分支用 MessageBoxW 反馈，不需要黑框）
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[macro_use]
mod logfile;
mod autostart;
mod callbacks;
mod config;
#[cfg(windows)]
mod drop_target;
mod dufs;
mod firewall;
mod i18n;
mod folder_picker;
mod job;
mod net;
mod qr;
mod service;
mod tray;
mod ui;
#[cfg(windows)]
mod window_finder;
#[cfg(windows)]
mod window_icon;

slint::include_modules!();

/// 主窗口标题。
///
/// ★ 它**只用于显示**（标题栏 / 任务栏 / Alt+Tab）。
///   找窗口一律走 `window_finder::main_hwnd()`（按 PID），**不再按标题找** ——
///   标题是要翻译的文案，一旦跟着语言变，按标题找窗口的五处会集体失效。
///   详见 `src/window_finder.rs` 开头的说明。
///
/// ★ 标题的真源是 `.slint` 里的 `window-title` 属性，这里注入。
///   批次 2 起这个函数会按 `Lang` 返回英文（见《实施计划》Step 11）。
pub fn main_window_title() -> String {
    i18n::t(i18n::K::AppName).to_string()
}

/// 单实例互斥量名（会话内可见即可）。
///
/// ★ 不用 `Global\` 前缀：建全局内核对象要 `SeCreateGlobalPrivilege`，
///   普通用户可能建不出来；托盘工具按会话单实例就够了。
///   改名即换锁，升级改算法时加个 `-v2` 后缀就能隔离老版本。
const SINGLE_INSTANCE_MUTEX_NAME: &str = "dufs-gui-single-instance-v1";

/// 拿住互斥量句柄的守卫：在 `main` 里活到进程结束。
/// 进程不管怎么死（含强杀），句柄都由系统回收，不留死锁。
#[cfg(windows)]
struct InstanceGuard(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

use crate::config::Config;
use crate::service::App;
// 图标相关代码在库那一层（`src/lib.rs`）—— 打包工具也用同一份
use dufs_gui::icon;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};
use std::sync::Arc;

fn main() {
    logfile::reset();
    // 带上 pid：日志里出现两套启动记录时，一眼就能判断是不是跑了两个实例
    flog!(
        "===== dufs-gui 启动 (dufs {}) pid={} =====",
        dufs::DUFS_VERSION,
        std::process::id()
    );

    // ---------- 0. 提权分支：加防火墙规则 ----------
    // 普通权限下 netsh 改不了防火墙策略，所以由 ShellExecuteW("runas") 再启动一份自己，
    // 那份实例只跑 netsh，跑完即退，不建界面。
    if let Some(port) = install_firewall_port_arg() {
        flog!("提权分支: 为端口 {port} 添加防火墙规则");
        std::process::exit(firewall::elevated_install(port));
    }

    // ---------- 0b. 提权分支：清理所有 dufs-gui 防火墙规则 ----------
    if std::env::args().any(|a| a == "--cleanup-firewall") {
        flog!("提权分支: 清理所有 dufs-gui 防火墙规则");
        std::process::exit(firewall::elevated_cleanup());
    }

    // ---------- 1. 单实例 ----------
    // 命名互斥量（内核对象）：已存在即说明有实例在跑。
    // 进程不管怎么退出（含强杀），句柄都由系统回收，不留死锁；
    // 也不存在“端口被陌生软件占用”这种误报，无需后台线程和握手。
    //
    // ★ 提权分支（--install-firewall / --cleanup-firewall）在上面就已 exit，
    //   不会走到这里抢锁，所以主界面开着时提权干活不受影响。
    let _instance_guard = match acquire_instance_guard() {
        Ok(guard) => guard,
        Err(()) => return, // 已有实例，提示过，直接退出
    };

    // ---------- 2. 配置 ----------
    // ★ 配置要**先读**,因为「详细日志」开关存在里面 —— 否则它后面那些
    //   vlog! 会因为标志位还没设好而全部记下来(表现为"关掉了还是写了一堆")。
    let cfg = Config::load();
    logfile::set_verbose(cfg.verbose_log);

    // ---------- 3. 渲染后端 ----------
    // 明确指定 winit + software 渲染器：不依赖 GPU/OpenGL，且体积最小。
    if let Err(e) = slint::BackendSelector::new()
        .backend_name("winit".to_string())
        .renderer_name("software".to_string())
        .select()
    {
        flog!("选择渲染后端失败，将使用默认后端: {e}");
    } else {
        vlog!("渲染后端: winit + software");
    }

    // ---------- 4. 窗口 ----------
    let ui = MainWindow::new().expect("创建窗口失败");

    // ★ 窗口/任务栏图标：填 .slint 里绑到 Window.icon 的 app-icon 属性。
    //   Slint 不会自动用 exe 资源里的图标。
    //
    //   ⚠ 但**光靠这里不够**：Slint 1.18 的 winit 后端有一段图标缓存判断 bug，
    //   对 `Image::from_rgba8` 这种没有 cache key 的图，它一次都不会调 WM_SETICON
    //   （详见 `src/window_icon.rs` 开头的完整推导与实测数据）。
    //   这里保留是因为它无害（同一份绘制数据），将来 Slint 修了也能自动生效；
    //   真正把标题栏图标设上去的是 show() 之后的 `window_icon::install()`。
    {
        let (rgba, w, h) = icon::app_icon_rgba();
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
        ui.set_app_icon(Image::from_rgba8(buf));
    }

    let app = Arc::new(App::new(cfg));

    // ★★ 界面语言必须在**这里**定下来，理由有三个：
    //   1. `select_bundled_translation()` 要求「首个组件已经创建」——
    //      语言包是随第一个组件的初始化注册进线程局部环境的，早于这一步调
    //      会返回 `NoTranslationsBundled`（Slint 的错误文档里明写了）；
    //   2. 它必须早于下面所有写界面的代码（标题 / 状态栏 / 提示语 / 关于表），
    //      否则窗口先以中文填好再被切走 —— 虽然 show() 还没调、看不见闪烁，
    //      但下面那些 `i18n::t()` 会**取到旧语言**，于是留下中文文案，必须避免；
    //   3. `apply_language` 要查「服务在不在跑、端口多少」来重写状态栏，
    //      所以 App 得先存在 —— 这也决定了它不能放到 MainWindow 那一步去。
    service::remember_app(&app);
    service::apply_language(&ui, app.cfg().language);

    app.push_cfg_to_ui(&ui);
    // ★ 配色要在 show() 之前定下来（窗口未创建时的设值会记成待应用属性，不会闪）。
    //   位置放在 push_cfg_to_ui 之后：那里已经把 theme-index 灌进界面。
    service::apply_theme(&ui, app.cfg().theme_mode);

    // 「开机自启」以**注册表里的真实状态**为准，而不是配置里那个字段：
    // 用户可能用任务管理器把启动项关掉了，配置就会和现实不一致。
    // 顺便让配置文件跟上现实（否则下次启动还是会显示成"已开"）。
    {
        let real = autostart::is_enabled();
        let cfg = app.cfg();
        if real != cfg.autostart {
            flog!(
                "开机自启的真实状态({real})与配置({})不一致，以注册表为准",
                cfg.autostart
            );
            app.set_autostart(real, cfg.autostart_serve, cfg.start_in_tray);
            app.save();
        } else if real && autostart::has_tray_flag() != cfg.start_in_tray {
            // 自启项在，但"静默进托盘"的开关跟注册表不一样（用户手改过注册表，
            // 或者老版本写的没有参数）—— 按配置重写一次，两边对齐。
            flog!(
                "注册表自启项的 --tray（{}）与配置（{}）不一致，按配置重写",
                autostart::has_tray_flag(),
                cfg.start_in_tray
            );
            match autostart::set_enabled(true, cfg.start_in_tray) {
                Ok(()) => {}
                Err(e) => flog!("重写自启项失败: {e}"),
            }
        }
        ui.set_autostart(real);
        ui.set_autostart_serve(cfg.autostart_serve);
        ui.set_start_in_tray(cfg.start_in_tray);
        if let Some(old) = autostart::registered_path()
            && !real
        {
            vlog!("注册表里的自启项指向别处（{old}），所以显示成未开启");
        }
    }

    // 「应用 → 关于」要显示的版本与路径,启动时填一次即可（UI 线程直设，同步生效）
    service::push_about_info(&ui);

    ui.set_running(false);
    ui.set_busy(false);
    ui.set_tab(Tab::Share);
    // 窗口标题由 Rust 注入（`.slint` 里 `title: root.window-title`）。
    // ★ 必须在 show() 之前：窗口还没创建时这个设值会被记成待应用属性，
    //   创建出来就是最终标题，不会看到“先显示旧标题再跳过去”。
    ui.set_window_title(main_window_title().into());
    ui.set_status_text(i18n::t(i18n::K::StatusNotStarted).into());
    // 提示语跟着「关闭行为」的设置走，别让用户自己猜 X 会干什么
    ui.set_hint_text(close_hint(app.cfg().close_action, app.cfg().confirm_on_close).into());
    ui.set_hint_is_error(false);

    // ---------- 5. 回调接线 ----------
    callbacks::wire_callbacks(&ui, &app);

    // ---------- 6. 探测网卡 + 生成二维码 ----------
    service::refresh_ips(&app, &ui.as_weak(), "启动");
    // 后台扫一下已有的防火墙放行规则（netsh 要 0.4~0.7 秒，绝不能卡住界面）
    service::refresh_firewall_rules(&ui.as_weak());

    // ---------- 7. 托盘（独立线程，绝不放在主线程）----------
    callbacks::spawn_tray(&ui, &app);

    // ---------- 8. 进入事件循环 ----------
    // ★ 这里不能用 ui.run()：
    //   ui.run() 走的是 run_event_loop()，在"最后一个窗口不可见"时会退出事件循环，
    //   于是"关闭窗口最小化到托盘"就变成了"关闭窗口直接退出"。
    //   run_event_loop_until_quit() 只有在显式调用 quit_event_loop() 时才退出，
    //   正好符合"常驻托盘"的语义。
    //
    // 窗口尺寸与位置由 Rust 侧控制（.slint 里不能写死 width/height，
    // 否则窗口会变成不可缩放）。
    // ★ 必须在 show() 之前：窗口还没创建时这几个设值会被记成待应用属性，
    //   创建出来就是最终状态，不会看到“先出现在默认位置再跳过去”。
    service::restore_window_state(&app, &ui);
    if let Err(e) = ui.show() {
        flog!("显示窗口失败: {e}");
    }

    // ---------- 静默启动（`--tray`，由「登录后自动运行」那一份自启项带上）----------
    // ★ 先 show() 再 hide()，而不是干脆不 show：窗口原生句柄（HWND）是
    //   拖放注册 / 标题栏图标 / 置顶的前提，没有它那三样都得失效。
    //   同一个事件循环回合里 show 完立刻 hide，系统还来不及画第一帧，看不到闪烁；
    //   以后从托盘恢复走 `force_full_repaint`，白屏问题也已经处理过。
    let start_hidden = std::env::args().any(|a| a == "--tray");
    if start_hidden {
        flog!("静默启动：直接进托盘，不显示窗口");
        let _ = ui.window().hide();
    }

    // ---------- 文件拖放（原生 IDropTarget，见 src/drop_target.rs）----------
    // 必须在 show() 之后：窗口没创建就没有 HWND 可注册。
    // 失败也不影响主流程，只是拖拽不可用（用户仍能用「选择」按钮）。
    #[cfg(windows)]
    callbacks::install_drop_target(&ui, &app);

    // ---------- 自动开始共享（配置里勾了才做）----------
    // 放在 show() 之后：确认框需要父窗口，而且它跑在子线程里。
    callbacks::maybe_autostart_serve(&ui, &app);

    // ---------- 窗口小图标（标题栏左上角）----------
    // ★ 不能用 Slint 的 Window.icon 代替 —— 那个在 1.18 上对代码画的图是失效的，
    //   原因和实测见 src/window_icon.rs。
    //   它自己也开线程等窗口（show() 返回时 HWND 还没建出来）。
    #[cfg(windows)]
    window_icon::install();

    if let Err(e) = slint::run_event_loop_until_quit() {
        flog!("事件循环异常退出: {e}");
    }
    flog!("事件循环已退出，开始整理");

    // ---------- 9. 兜底清理 ----------
    // 正常路径下 service::quit() 已经清理过了，这里只是防止意外退出时留下 dufs。
    // 就算这一步没跑到（比如进程被强杀），Job Object 也会兜住。
    // 看门狗：正常路径下 quit() 已经装了，这里是双保险（意外退出时也能兜）。
    arm_exit_watchdog();
    let weak = ui.as_weak();
    service::stop_service(&app, &weak);
    flog!("兜底清理完成，准备关托盘并退出");
    tray::shutdown();

    // ---------- 10. 直接终止进程（不走 ExitProcess）----------
    //
    // ★ 为什么不 return 让进程自然结束：
    //   `ExitProcess` 会在**主线程持加载器锁**时给每个已加载的 DLL 跑
    //   `DLL_PROCESS_DETACH`。只要此时还有别的线程卡在需要加载器锁的调用里
    //   （shell32 / ole32 / comctl32 / 杀软注入的 DLL …），两边就互等死锁，
    //   进程**停在退出阶段不退**。而它此刻仍是**前台进程**，Windows 就会让
    //   别的窗口（终端 / VS Code）排队等它 —— 用户看到的就是「关掉之后整个
    //   电脑像卡了几秒」。这一段发生在 main 返回之后，**日志里永远看不到**。
    //
    // ★ 为什么在这里直接 TerminateProcess 是安全的：
    //   - 日志每行都是 open → write → close（`logfile.rs::log`），配置是显式
    //     `Config::save()`，托盘图标也已 NIM_DELETE 并等它完成（`tray::shutdown`）；
    //   - dufs 已经停止，就算漏了，Job Object 的 KILL_ON_JOB_CLOSE 也会兜住；
    //   - `TerminateProcess` **不做 DLL 分离**，从根上避开上面那个死锁，
    //     退出耗时是毫秒级，也不再占着前台把别的程序拖住。
    //   `arm_exit_watchdog()` 仍保留：万一连这一步都没跑到（比如卡在更早的
    //   清理里），3 秒后由它硬杀。
    flog!("清理完成，直接终止进程（跳过 ExitProcess 的 DLL 卸载，避免退出阶段死锁）");
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // 成功时**不返回**（进程就此结束）；能返回就说明失败了，退回常规退出。
        let _ = TerminateProcess(GetCurrentProcess(), 0);
    }
    flog!("TerminateProcess 未生效，退回常规退出（此时仍可能卡在 ExitProcess）");
}

// ============================================================
// 退出看门狗
// ============================================================

/// 开始退出时调一次：从此刻起 3 秒内进程还没结束，就硬杀自己。
///
/// ★ 为什么要这么干：Win32 的进程退出并不是“一定成功”。
///   `ExitProcess` 会在**主线程持加载器锁**的状态下给每个 DLL 跑
///   `DLL_PROCESS_DETACH`；只要还有别的线程卡在需要加载器锁的地方
///   （shell32 / ole32 / `LoadImageW` / CRT 锁…），就会**死锁**。
///
///   后果很难看：窗口已经 hide 了，但进程还在 —— 而 `hide()` 并不放弃前台，
///   Windows 仍把它当**前台进程**，于是别的程序点不进来、键盘也没反应。
///   用户看到的就是“关掉之后 VS Code 卡住 / 电脑像死机”。
///
///   这时 `TerminateProcess` 是唯一可靠的路：它**不做 DLL 分离**，不会死锁。
///   走到这一步该做的清理都已经做完了（停 dufs、摘托盘图标、存配置），
///   所以硬杀是安全的；而且宁可硬杀，也不能留个僵尸进程占着前台。
pub(crate) fn arm_exit_watchdog() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ARMED: AtomicBool = AtomicBool::new(false);
    if ARMED.swap(true, Ordering::SeqCst) {
        return; // 只装一次
    }
    std::thread::Builder::new()
        .name("exit-watchdog".into())
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(3));
            flog!("退出超时（3 秒）—— 正常清理流程卡住了，强制结束进程");
            #[cfg(windows)]
            unsafe {
                use windows::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
                let _ = TerminateProcess(GetCurrentProcess(), 0);
            }
            // 非 Windows，或 TerminateProcess 也失败：退回常规退出
            std::process::exit(0);
        })
        .ok();
}

/// 底部那行提示：说明 X 到底会干什么（动作 × 问不问，四种组合）
pub(crate) fn close_hint(action: crate::config::CloseAction, confirm: bool) -> &'static str {
    use crate::config::CloseAction;
    match (action, confirm) {
        (_, true) => i18n::t(i18n::K::HintCloseAsk),
        (CloseAction::Tray, false) => i18n::t(i18n::K::HintCloseTray),
        (CloseAction::Quit, false) => i18n::t(i18n::K::HintCloseQuit),
    }
}

/// 解析 `--install-firewall <port>`
fn install_firewall_port_arg() -> Option<u16> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--install-firewall" {
            return args.next().and_then(|v| v.parse::<u16>().ok());
        }
    }
    None
}

/// 拿单实例锁。
///
/// 返回：
///   - `Ok(guard)`：首实例，`guard` 必须在 `main` 里活到进程结束；
///   - `Err(())`：互斥量已存在，真有实例在跑，已弹窗，调用方直接退出。
///
/// ★ 只有“建锁失败”这种极端情况才放行启动（没有锁总比打不开强）。
#[cfg(windows)]
fn acquire_instance_guard() -> Result<Option<InstanceGuard>, ()> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::HSTRING;

    let name = HSTRING::from(SINGLE_INSTANCE_MUTEX_NAME);
    let handle = unsafe {
        match CreateMutexW(None, false, &name) {
            Ok(h) => h,
            Err(e) => {
                flog!("单实例互斥量创建失败（{e}），跳过单实例保护继续启动");
                return Ok(None);
            }
        }
    };
    // CreateMutexW 成功时也要查 GetLastError：
    // 返回已存在的句柄 + ERROR_ALREADY_EXISTS 才是“有实例”，
    // 光看句柄非空不够。
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        flog!("单实例检查：互斥量已存在，判定已有实例在运行，退出");
        firewall::message_box(
            i18n::t(i18n::K::AlreadyRunning),
            i18n::t(i18n::K::AppName),
            false,
        );
        return Err(());
    }
    Ok(Some(InstanceGuard(handle)))
}

/// 非 Windows 纯占位：本项目只发 Windows 版，其他平台直接放行。
#[cfg(not(windows))]
fn acquire_instance_guard() -> Result<Option<()>, ()> {
    Ok(None)
}

