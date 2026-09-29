//! 系统托盘
//!
//! ★★★ 本模块有两个必须遵守的约束，否则会出现难以定位的 bug ★★★
//!
//! 1) **托盘必须创建在独立线程里。**
//!    tray-icon 在 Windows 上弹出右键菜单时调用的 `TrackPopupMenu` 是一个
//!    *同步模态* API，它会阻塞调用线程直到菜单关闭。如果建在主线程（Slint 的
//!    事件循环所在线程），菜单一打开整个界面就冻住。
//!    官方 issue：tauri-apps/tray-icon#358
//!
//! 2) **必须手动给菜单挂 WM_COMMAND 子类。**
//!    tray-icon 只把 HMENU 交给 `TrackPopupMenu`，它**不会**调用 muda 的
//!    `init_for_hwnd`。而菜单项被点击时发出的是 `WM_COMMAND`，默认窗口过程
//!    不处理，于是 `MenuEvent` 永远不会触发 —— 表现为"点菜单毫无反应"。
//!    所以要显式调用 `menu.attach_menu_subclass_for_hwnd(托盘窗口句柄)`。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// 托盘线程 id（用于 PostThreadMessageW 唤醒/退出该线程）
static TRAY_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// 托盘线程的"已退出"信号。`shutdown()` 靠它**等**线程真做完清理（而不是赌 sleep）。
static TRAY_DONE: Mutex<Option<mpsc::Receiver<()>>> = Mutex::new(None);

/// 期望的"服务运行中"状态，用来更新托盘提示文字
static WANT_RUNNING: AtomicBool = AtomicBool::new(false);

/// 自定义消息：刷新托盘提示
const WM_REFRESH: u32 = 0x8000 + 1; // WM_APP + 1

/// 托盘菜单的动作。用 Box<dyn Fn> 传进来，避免 tray 模块反过来依赖 App 类型。
pub struct Actions {
    pub show: Box<dyn Fn() + Send + Sync>,
    pub stop: Box<dyn Fn() + Send + Sync>,
    pub copy_url: Box<dyn Fn() + Send + Sync>,
    pub quit: Box<dyn Fn() + Send + Sync>,
}

/// 启动托盘线程
pub fn spawn(actions: Actions) {
    let actions = Arc::new(actions);
    let (tx, rx) = mpsc::channel::<()>();
    *TRAY_DONE.lock().expect("tray done lock") = Some(rx);

    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            if let Err(e) = run(actions) {
                flog!("托盘线程异常退出: {e}");
            }
            // 通知 shutdown()：图标已经摘干净了，可以放心退进程
            let _ = tx.send(());
        })
        .expect("无法创建托盘线程");
}

#[cfg(windows)]
fn run(actions: Arc<Actions>) -> Result<(), String> {
    use tray_icon::menu::{ContextMenu, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };

    // ---------- 1. 菜单 ----------
    let menu = Menu::new();
    let item_open = MenuItem::with_id("open", crate::i18n::t(crate::i18n::K::TrayMenuOpen), true, None);
    let item_stop = MenuItem::with_id("stop", crate::i18n::t(crate::i18n::K::TrayMenuStop), true, None);
    let item_copy =
        MenuItem::with_id("copy-url", crate::i18n::t(crate::i18n::K::TrayMenuCopyUrl), true, None);
    let item_quit = MenuItem::with_id("quit", crate::i18n::t(crate::i18n::K::TrayMenuQuit), true, None);

    menu.append(&item_open).map_err(|e| e.to_string())?;
    menu.append(&item_stop).map_err(|e| e.to_string())?;
    menu.append(&item_copy).map_err(|e| e.to_string())?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| e.to_string())?;
    menu.append(&item_quit).map_err(|e| e.to_string())?;

    // ---------- 2. 菜单事件 -> Actions ----------
    {
        let actions = actions.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            flog!("托盘菜单点击: {}", event.id.as_ref());
            match event.id.as_ref() {
                "open" => (actions.show)(),
                "stop" => (actions.stop)(),
                "copy-url" => (actions.copy_url)(),
                "quit" => (actions.quit)(),
                _ => {}
            }
        }));
    }

    // ---------- 2b. 双击图标 = 打开界面 ----------
    // 所有人的肌肉记忆；必须另开一个事件处理器（菜单事件和图标事件是两套）。
    {
        use tray_icon::{MouseButton, TrayIconEvent};
        let actions = actions.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::DoubleClick { button, .. } = event
                && button == MouseButton::Left
            {
                flog!("托盘图标双击 -> 打开界面");
                (actions.show)();
            }
        }));
    }

    // ---------- 3. 托盘图标 ----------
    let (rgba, w, h) = crate::icon::app_icon_rgba();
    let icon = Icon::from_rgba(rgba, w, h).map_err(|e| format!("图标数据无效: {e}"))?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_icon(icon)
        .with_tooltip(crate::i18n::tray_tooltip(false))
        .with_menu_on_left_click(false) // 左键单击不弹菜单（双击=打开界面，见事件处理器）
        .build()
        .map_err(|e| format!("创建托盘图标失败: {e}"))?;

    // ---------- 4. ★ 挂上 WM_COMMAND 子类（少了这一步菜单点了没反应）----------
    // attach_menu_subclass_for_hwnd 是 muda 的 ContextMenu trait 方法，需要 trait 在作用域内。
    let hwnd = tray.window_handle() as isize;
    unsafe {
        menu.attach_menu_subclass_for_hwnd(hwnd);
    }

    TRAY_THREAD_ID.store(thread_id(), Ordering::SeqCst);

    // 记录托盘图标位置，便于自动化测试/排查（physical 像素）
    let rect_desc = match tray.rect() {
        Some(r) => format!(
            "x={} y={} w={} h={}",
            r.position.x, r.position.y, r.size.width, r.size.height
        ),
        None => "<未知，可能被收纳进溢出区>".to_string(),
    };
    vlog!("托盘图标已创建: {rect_desc}");

    // ---------- 5. 消息泵 ----------
    // 本线程会被 TrackPopupMenu 阻塞，这正是我们想要的：只冻托盘线程，不冻界面。
    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !ret.as_bool() {
            break; // WM_QUIT
        }

        if msg.message == WM_REFRESH {
            let running = WANT_RUNNING.load(Ordering::SeqCst);
            let _ = tray.set_tooltip(Some(crate::i18n::tray_tooltip(running)));
            item_stop.set_enabled(running);
            // 菜单文字也在这里重设：切界面语言时主线程只能叫醒本线程，
            // 不能直接碰托盘对象（它们属于这个线程）。
            // 😅 这就是为什么菜单不用每次重建 —— muda 的 MenuItem 支持改文字。
            item_open.set_text(crate::i18n::t(crate::i18n::K::TrayMenuOpen));
            item_stop.set_text(crate::i18n::t(crate::i18n::K::TrayMenuStop));
            item_copy.set_text(crate::i18n::t(crate::i18n::K::TrayMenuCopyUrl));
            item_quit.set_text(crate::i18n::t(crate::i18n::K::TrayMenuQuit));
            continue;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // ---------- 6. 收尾：让托盘图标立刻从通知区消失 ----------
    flog!("托盘线程收到退出消息，正在移除图标");
    let _ = tray.set_visible(false);
    unsafe {
        menu.detach_menu_subclass_from_hwnd(hwnd);
    }
    drop(tray);
    drop(menu);

    Ok(())
}

#[cfg(not(windows))]
fn run(_actions: Arc<Actions>) -> Result<(), String> {
    Err("当前平台未实现托盘（仅 Windows）".into())
}

/// 告诉托盘线程：服务状态变了，刷新提示文字
pub fn set_running(running: bool) {
    WANT_RUNNING.store(running, Ordering::SeqCst);
    wake();
}

/// 告诉托盘线程：界面语言换了，重新设一遍菜单与提示文字。
///
/// ★ 菜单项文字是**创建时**写进去的，改语言后必须重设；
///   但它们归托盘线程所有（ComCtl32 的菜单不是线程安全的），
///   所以不能在这里直接改，只能 `wake()` 叫醒那个线程。
pub fn refresh_text() {
    wake();
}

/// 请求托盘线程退出并清理图标，**并等它真做完**。
///
/// ★ 为什么不能只 `sleep(250ms)` 就完事（以前就是这么写的，会卡死电脑）：
///   托盘线程退出前要调 `Shell_NotifyIcon(NIM_DELETE)` 把图标从通知区摘掉，
///   那是**跨进程同步调用** —— 要等 Explorer 回话，可能几百毫秒到几秒。
///   如果它还没做完主线程就走到 `main` 返回，`ExitProcess` 会在加载器锁里
///   给每个 DLL 跑 `DLL_PROCESS_DETACH`，而托盘线程正卡在 shell32 里 ——
///   进程挂在退出阶段，Explorer 的通知区（连带任务栏/桌面）也因为等我们
///   回话而一起卡住。所以这里必须**等到线程结束或超时**。
///
/// 超时也走：宁可偶尔留个幽灵图标，也不能让程序永远退不出去（3 秒内正常
/// 情况早就完了，超时本身就该记一笔日志查）。
pub fn shutdown() {
    #[cfg(windows)]
    {
        let tid = TRAY_THREAD_ID.load(Ordering::SeqCst);
        if tid != 0 {
            // 置 0：本函数可能被调两次（关窗退出路径 + 主循环收尾），
            // 第二次应该直接变成空操作，不再白等。
            TRAY_THREAD_ID.store(0, Ordering::SeqCst);
            unsafe {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }

        let rx = TRAY_DONE.lock().ok().and_then(|mut r| r.take());
        let Some(rx) = rx else { return };
        let t0 = std::time::Instant::now();
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(()) => flog!("托盘线程已退出（等了 {} ms）", t0.elapsed().as_millis()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                flog!("托盘线程早已结束（等了 {} ms）", t0.elapsed().as_millis())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => flog!(
                "等托盘线程退出超时（已等 {} ms）—— 它可能卡在 Shell_NotifyIcon 里，\
                 继续退进程（通知区可能留个幽灵图标）",
                t0.elapsed().as_millis()
            ),
        }
    }
}

fn wake() {
    #[cfg(windows)]
    {
        let tid = TRAY_THREAD_ID.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
                let _ = PostThreadMessageW(tid, WM_REFRESH, WPARAM(0), LPARAM(0));
            }
        }
    }
}

#[cfg(windows)]
fn thread_id() -> u32 {
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}
