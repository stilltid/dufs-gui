//! 文件夹选择对话框
//!
//! 两个关键点：
//!
//! 1) **必须指定 parent 窗口。**
//!    不指定的话，rfd 创建的是一个"无主"对话框：它不模态于我们的窗口，
//!    可能出现在窗口后面、或者焦点乱跑，用户感觉就是"点了半天没反应"。
//!    rfd 的 `set_parent` 需要一个 `HasWindowHandle`，而 Slint 没有直接暴露
//!    原生窗口句柄，所以这里先拿到主窗口 HWND（`window_finder::main_hwnd()`，
//!    **按 PID 找**），再包一层假的 `HasWindowHandle` 传进去。
//!
//!    ★ 原来这里按窗口标题 `FindWindowW` 找。标题是要翻译的文案，一翻译
//!    「英文标题 != 中文标题」→ 找不到窗口 → 对话框变无主。改用
//!    `src/window_finder.rs` 的 PID 枚举后，与标题彻底无关。
//!
//! 2) **绝不能在子线程里写 Slint 属性。**
//!    Slint 的属性只能由 UI 线程修改，跨线程写不会立刻触发重绘 ——
//!    表现就是"选完文件夹，输入框过一会儿才变"。所以本模块只负责"拿到路径"，
//!    界面更新交给调用方用 `ui::ui_set()` 回投。

use std::path::PathBuf;


/// 弹出"选择文件夹"对话框。`start_dir` 是打开时的初始目录。
///
/// 返回值：用户选中了路径 -> `Some(path)`；取消 -> `None`。
pub fn pick_folder(start_dir: Option<PathBuf>, title: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);

    if let Some(dir) = start_dir
        && dir.is_dir()
    {
        dialog = dialog.set_directory(dir);
    }

    // 把对话框挂到主窗口上（拿不到句柄就退化为无主对话框，不影响功能）
    #[cfg(windows)]
    if let Some(parent) = RawWin::main() {
        dialog = dialog.set_parent(&parent);
    }

    dialog.pick_folder()
}

/// 弹出"选择文件"对话框（用于挑证书 / 私钥）。
///
/// `filter` 是 (描述, 扩展名列表)，例如 `("PEM 证书", &["pem", "crt", "cer"])`。
pub fn pick_file(
    start_dir: Option<PathBuf>,
    title: &str,
    filter: (&str, &[&str]),
) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);

    if !filter.1.is_empty() {
        dialog = dialog.add_filter(filter.0, filter.1);
    }
    if let Some(dir) = start_dir
        && dir.is_dir()
    {
        dialog = dialog.set_directory(dir);
    }
    #[cfg(windows)]
    if let Some(parent) = RawWin::main() {
        dialog = dialog.set_parent(&parent);
    }

    dialog.pick_file()
}

/// 弹一个「是 / 否」确认框，挂到主窗口上。用户点「是」返回 true。
///
/// 只用于**不可逆**的操作（删除程序数据、清理防火墙规则），
/// 所以默认用 Warning 级别。
pub fn confirm(title: &str, text: &str) -> bool {
    let mut dialog = rfd::MessageDialog::new()
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::YesNo)
        .set_level(rfd::MessageLevel::Warning);

    #[cfg(windows)]
    if let Some(parent) = RawWin::main() {
        dialog = dialog.set_parent(&parent);
    }

    matches!(dialog.show(), rfd::MessageDialogResult::Yes)
}

// ============================================================
// 下面是为了把 HWND 交给 rfd 而做的一层薄封装
// ============================================================

#[cfg(windows)]
mod parent_handle {
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
        Win32WindowHandle, WindowHandle,
    };
    use std::num::NonZeroIsize;

    use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, GWLP_HINSTANCE};

    /// 一个持有原始 HWND 的 `HasWindowHandle` 实现。
    ///
    /// HWND 只是内核对象句柄，跨线程传递是安全的，所以显式声明 Send。
    pub struct RawWin {
        hwnd: isize,
        hinstance: Option<NonZeroIsize>,
    }

    unsafe impl Send for RawWin {}
    unsafe impl Sync for RawWin {}

    impl RawWin {
        /// 拿主窗口（**按 PID 找**，见 `crate::window_finder`）。找不到返回 None。
        ///
        /// 为什么要顺带取 `hinstance`：`Win32WindowHandle` 里这个字段
        /// 允许为空，但 rfd 内部走的是 `GetWindowLongPtrW(GWLP_HINSTANCE)`
        /// 的同一套取值，填上更接近“真窗口”的样子。
        pub fn main() -> Option<Self> {
            let raw = crate::window_finder::main_hwnd()?;
            let hwnd = windows::Win32::Foundation::HWND(raw as *mut core::ffi::c_void);
            let hinstance = NonZeroIsize::new(unsafe { GetWindowLongPtrW(hwnd, GWLP_HINSTANCE) });
            Some(Self { hwnd: raw, hinstance })
        }
    }

    impl HasWindowHandle for RawWin {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let mut handle = Win32WindowHandle::new(NonZeroIsize::new(self.hwnd).unwrap());
            handle.hinstance = self.hinstance;
            // SAFETY: hwnd 来自 `window_finder`（本进程的顶层窗口），在本函数
            //         返回瞬间仍然有效；调用方（rfd）只会在对话框存活期间用它。
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
        }
    }

    impl HasDisplayHandle for RawWin {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            // Windows 的 display handle 本身不携带数据
            Ok(DisplayHandle::windows())
        }
    }
}

#[cfg(windows)]
use parent_handle::RawWin;
