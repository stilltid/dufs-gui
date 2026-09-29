//! 按 **PID** 找本进程的主窗口 —— 与窗口标题彻底解耦。
//!
//! ★ 为什么不能按标题找窗口
//!   原来的做法是 `FindWindowW(None, &HSTRING::from("dufs 文件共享"))`。
//!   但窗口标题是**要翻译的界面文案**（中文「dufs 文件共享」/ 英文
//!   「dufs File Sharing」），标题一跟着语言走，下面五处会**集体静默失效**
//!   （都是"找不到窗口"就跳过/降级，不报错）：
//!
//!   | 谁 | 失效后的表现 |
//!   |---|---|
//!   | `folder_picker`（对话框 parent） | 对话框变无主：可能出现在窗口后面、焦点乱跑 |
//!   | `window_icon`（标题栏小图标） | 标题栏左上角一直空白 |
//!   | `drop_target`（原生拖放注册） | 拖文件进窗口没反应 |
//!   | `service::set_topmost`（窗口置顶） | 报「找不到主窗口」 |
//!   | `firewall::focus_main_window`（托盘「打开界面」） | 点了没反应，窗口不往前跳 |
//!
//!   按 PID 找则**与标题无关**，上面五处一起解耦。
//!
//! ★ 只认自己这个进程
//!   `GetWindowThreadProcessId == GetCurrentProcessId()`，所以不会误抓到
//!   别人的同名窗口（比如用户同时开着两个不同的程序、标题恰好一样）。
//!
//! ★ 窗口标题仍然存在，但它只剩**显示**用途
//!   见 `main.rs::main_window_title()` 与 `ui/app.slint` 的 `window-title`。

use std::time::Duration;

/// 启动期轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// 启动期最多等多久窗口出现。
///
/// ★ 为什么必须等：`ui.show()` 返回时原生窗口**还没建出来**
///   （winit 要等事件循环真转起来才创建 HWND），此刻枚举是空的。
///   而又**不能**在 UI 线程上死等 —— 那样事件循环永远起不来，窗口也就
///   永远建不出来。所以调用方开一条子线程来调 `wait_main_hwnd()`。
///
/// 5 秒等不到就放弃：只影响「标题栏图标 / 文件拖放」这类锦上添花的功能，
/// 不打断启动。
pub const STARTUP_WAIT: Duration = Duration::from_secs(5);

/// 系统标准对话框的窗口类名。
///
/// ★ 为什么必须排除它（这是个真竞态，不是理论问题）：
///   `TaskDialog` / `MessageBox` 和 rfd 的文件对话框（Vista 的 `IFileDialog`）
///   **都是 `#32770`，而且都建在我们自己的进程里** —— 所以它们也满足
///   「本进程 + 可见 + 有标题」这三个条件。
///
///   而启动期有两条轮询线程在找窗口（`window_icon` 和 `install_drop_target`），
///   它们最多等 5 秒；同时 `service::maybe_autostart_serve` 会在 400ms 后
///   从后台线程弹一个「自动开始共享」确认框。两者时间上**可能重叠**：
///   万一枚举到那个对话框，就会把标题栏图标设到对话框上、
///   或把拖放注册到对话框上（而不是主窗口）。
///
///   按 PID 找解决的是「标题被翻译 → 找不到」；类名过滤解决的是
///   「找到了但找到的是对话框」—— 两件不同的事，都要做。
const SYSTEM_DIALOG_CLASS: &str = "#32770";

/// 主窗口 HWND（`isize`）。找不到返回 `None`。
///
/// 候选必须同时满足：**属于本进程 + 可见 + 不是 tool window**。
/// 其中有标题的优先（winit 的消息窗口/辅助窗口通常没有标题），
/// 一个都没有标题时退一步取「本进程第一个可见的顶层窗口」——
/// 启动早期标题可能还没写上去，但句柄已经是同一个了。
pub fn main_hwnd() -> Option<isize> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    use windows::core::BOOL;

    /// 枚举过程中收集到的候选。
    #[derive(Default)]
    struct Found {
        /// 最好的：本进程 + 可见 + 有标题
        titled: Option<isize>,
        /// 退路：本进程 + 可见
        visible: Option<isize>,
    }

    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam 是 main_hwnd() 里传进来的 &mut Found。EnumWindows 是
        //         同步调用，回调只在这次调用期间被调用，所以这个引用的生命周期
        //         一定覆盖得住。
        let found = unsafe { &mut *(lparam.0 as *mut Found) };
        if let Some((raw, titled)) = candidate(hwnd) {
            if titled {
                found.titled = Some(raw);
                return BOOL(0); // 已经拿到最好的，不用再往下枚举
            }
            if found.visible.is_none() {
                found.visible = Some(raw);
            }
        }
        BOOL(1) // 继续枚举
    }

    let mut found = Found::default();
    // 提前停（回调返回 FALSE）会让 EnumWindows 返回 Err，忽略即可 ——
    // 那不是"失败"，正是我们要的"找到了"。
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize)) };
    found.titled.or(found.visible)
}

/// 轮询等主窗口出现，最多等 `timeout`。超时返回 `None`。
///
/// 给启动期用（`window_icon::install` / `callbacks::install_drop_target`）：
/// 那时窗口还没建出来，所以调用方在子线程里调，不能在 UI 线程上等。
pub fn wait_main_hwnd(timeout: Duration) -> Option<isize> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(hwnd) = main_hwnd() {
            return Some(hwnd);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 这个顶层窗口算不算候选？返回 `(hwnd, 有没有标题)`。
///
/// 四个条件缺一不可：属于本进程、可见、不是 tool window、**不是系统对话框**。
fn candidate(hwnd: windows::Win32::Foundation::HWND) -> Option<(isize, bool)> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsWindowVisible, WS_EX_TOOLWINDOW,
    };

    unsafe {
        // 只认自己进程的窗口
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != std::process::id() {
            return None;
        }
        // 隐藏窗口不是主窗口（托盘图标、winit 的消息窗口、Slint 的辅助窗口都在这类）
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        // tool window 不出现在任务栏，也不是我们的主窗口
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        // 系统对话框（`#32770`）也不是主窗口 —— 详见 SYSTEM_DIALOG_CLASS 的推导
        if is_system_dialog(hwnd) {
            return None;
        }
        // 有标题的优先：winit 的消息窗口/辅助窗口通常没有标题。
        // 注意不能用"必须有标题"来**排除** —— 启动早期 Slint 可能还没把
        // title 属性写下去，那时窗口是合法的、只是暂时无标题。
        let titled = GetWindowTextLengthW(hwnd) > 0;
        Some((hwnd.0 as isize, titled))
    }
}

/// 这个窗口是不是系统标准对话框（类名 `#32770`）。
fn is_system_dialog(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;

    // 32 个 UTF-16 单元足够装下所有真实类名（`#32770`、`WinUIDesktopWin32WindowClass` …）；
    // 装不下时 API 返回 0，那就当成"不是对话框"，不影响主路径。
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return false;
    }
    String::from_utf16_lossy(&buf[..n as usize]) == SYSTEM_DIALOG_CLASS
}

#[cfg(test)]
mod tests {
    //! 确定性单测：**不依赖 Slint / winit**，自己建一个真窗口，验证
    //! 「按 PID 能找到本进程的顶层窗口」。
    //!
    //! 为什么要它：Step 1 把 5 处 `FindWindowW(标题)` 换成了按 PID 枚举，
    //! 而那 5 处（对话框 parent / 标题栏图标 / 拖放注册 / 窗口置顶 /
    //! 托盘抢前台）全都靠 `main_hwnd()` 这一个函数。GUI 手测之外，
    //! 这里给出可重复、不需要人盯屏幕的证据。
    //!
    //! ★ 顺带证明「与标题无关」：窗口标题故意用中文，而函数里没有任何
    //!   跟标题内容有关的判断（只看**有没有**标题，不看写了什么）。
    use super::main_hwnd;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_SHOW, ShowWindow,
        WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// 自己的窗口过程。
    ///
    /// ★ 不能直接把 `DefWindowProcW` 当 `lpfnWndProc`：Windows 要求的是
    ///   `extern "system"` 调用约定，而 windows-rs 把 `DefWindowProcW`
    ///   生成成了 Rust 调用约定的包装函数（类型对不上，编译期就报错）。
    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// 保证不管怎么退出（包括断言失败 panic）都把测试窗口销毁掉
    struct TempWindow(HWND);
    impl Drop for TempWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn 按pid能找到本进程的顶层窗口() {
        let class = HSTRING::from("dufs_gui_window_finder_test");
        let title = HSTRING::from("dufs 文件共享");

        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(None).expect("GetModuleHandleW");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            // 同一个进程里重复注册同一个类会返回 0，无所谓（测试只跑一次）
            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                Default::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                200,
                100,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .expect("CreateWindowExW");
            let _ = ShowWindow(hwnd, SW_SHOW);
            hwnd
        };
        let _win = TempWindow(hwnd);

        let found = main_hwnd().expect("main_hwnd() 应当能枚举到本进程刚建的可见顶层窗口");
        assert_eq!(
            found, hwnd.0 as isize,
            "main_hwnd() 返回了别的窗口：期望我们刚建的那个"
        );
    }
}
