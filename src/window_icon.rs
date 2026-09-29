//! 窗口左上角（标题栏）的小图标。
//!
//! # 为什么不能只靠 Slint 的 `Window.icon`
//!
//! Slint 1.18 的 winit 后端在 `update_window_properties()` 里有一段
//! 「图标没变就别重设，免得闪一下」的缓存判断
//! （`i-slint-backend-winit-1.18.0/src/winitwindowadapter.rs` 第 1874 行起）：
//!
//! ```ignore
//! let icon_image_cache_key = ImageCacheKey::new((&icon_image).into());
//! if *self.window_icon_cache_key.borrow() != icon_image_cache_key {   // ★
//!     *self.window_icon_cache_key.borrow_mut() = icon_image_cache_key;
//!     winit_window_or_none.set_window_icon(icon_to_winit(...));
//! }
//! ```
//!
//! 而 `ImageCacheKey::new()` 对 `ImageInner::EmbeddedImage { cache_key, .. }`
//! 返回的就是那个 `cache_key`；`Image::from_rgba8()` 造出来的图是
//! `cache_key: ImageCacheKey::Invalid`，`new()` 于是返回 **`None`**。
//! 缓存字段 `window_icon_cache_key` 的初始值也是 `None` ——
//! `None == None` 成立，**`★` 那个分支一次都不会进**，`WM_SETICON` 从来没被调用过。
//!
//! 实测（`WM_GETICON` + `GetClassLongPtrW`）确实是全 0：
//!
//! ```text
//! class=Window Class  title=[dufs 文件共享]
//!   WM_GETICON ICON_SMALL(0)=0x0  ICON_BIG(1)=0x0  ICON_SMALL2(2)=0x0
//!   GetClassLong GCLP_HICON=0x0   GCLP_HICONSM=0x0
//! ```
//!
//! 所以：**用 `@image-url` 加载的图（有真实 cache key）当窗口图标能生效，
//! 用代码画的像素图（`Image::from_rgba8`）会静默失效**。
//!
//! 这也正好解释了那个"玄学"现象 —— 任务栏有图标、标题栏空白：
//! 任务栏拿不到窗口大图标时会**回落**到 exe 的文件图标（所以看着是好的），
//! 标题栏画的 `ICON_SMALL` 没有回落机制，就只能是空白。
//!
//! # 这里的做法
//!
//! 绕开 Slint，在 `show()` 之后自己 `WM_SETICON` 设 **ICON_SMALL**：
//!   - `ICON_SMALL` 正是标题栏用的那一档（`ICON_BIG` 留给任务栏/Alt+Tab，
//!     不设它，Shell 就能继续从 exe 的多尺寸 `.ico` 里挑，比我们塞一张
//!     32px 的位图上去更清楚）；
//!   - 顺带 `SetClassLongPtrW(GCLP_HICONSM)`，这样以后新建的同类窗口、
//!     以及 `WM_GETICON(ICON_SMALL2)` 也有图标；
//!   - 图标优先从**自己 exe 的图标资源**里按目标尺寸取（`icon_tool embed`
//!     写进去的那份，7 档尺寸，Windows 会挑最合适的一档），
//!     取不到（比如 `cargo run` 出来的、还没 embed 的 exe）就现场用
//!     `src/icon.rs` 的绘制数据造一个，保证开发时也看得到；
//!   - 整件事放在一条线程里轮询等窗口出现，理由见 `install()` 的注释。

#[cfg(windows)]
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIcon, GetSystemMetrics, LoadImageW, SendMessageW, SetClassLongPtrW, GCLP_HICONSM,
    HICON, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, SM_CXSMICON, SM_CYSMICON,
    WM_SETICON,
};

/// 给主窗口装上标题栏小图标。
///
/// ★ 为什么要开一条线程轮询：`ui.show()` 返回时原生窗口**还没建出来**
///   （winit 要等事件循环真转起来才创建 HWND），此刻枚举是空的。
///   而**不能**在主线程上死等 —— 那样事件循环永远起不来，窗口也就永远建不出来。
///   `callbacks::install_drop_target` 出于同一个原因用的是同一套等待。
///
/// ★ 找窗口按 **PID**（`window_finder`），**不按标题** —— 标题是要翻译的
///   界面文案，按标题找的话英文界面下这里会静默跳过，标题栏一直空白。
///
/// 实测：紧跟在 `show()` 后面直接调，窗口还没建出来，枚举是空的。
pub fn install() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        // 窗口一般在几百毫秒内建好；等不到就放弃（只影响标题栏图标，不打断启动）。
        let Some(raw) = crate::window_finder::wait_main_hwnd(crate::window_finder::STARTUP_WAIT)
        else {
            flog!("窗口小图标：等不到主窗口，跳过（标题栏将没有图标）");
            return;
        };
        // 切回 UI 线程再设：`GetSystemMetrics(SM_CXSMICON)` 是按**调用线程**的
        // DPI 环境算的，非 UI 线程拿到的可能是错的尺寸。
        let _ = slint::invoke_from_event_loop(move || set_small_icon(HWND(raw as *mut _)));
    });
}

#[cfg(windows)]
fn set_small_icon(hwnd: HWND) {
    // 标题栏图标是"小图标"尺寸，而且它随 DPI 变（100% = 16px，150% = 24px），
    // 所以要问系统要当前值，不能写死 16。
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };

    let hicon = match from_exe_resource(cx, cy) {
        Some(h) => h,
        None => {
            // 走到这里说明 exe 里没有图标资源 —— 大概率是忘了打包那一步：
            //   cargo run --bin icon_tool -- embed <exe>
            // 不影响启动，但标题栏会退化成"没有图标"，所以记一条日志方便排查。
            flog!("窗口小图标：exe 里没有图标资源（忘了跑 icon_tool embed？），改用绘制数据现场生成");
            match from_drawn_pixels(cx) {
                Some(h) => h,
                None => {
                    flog!("窗口小图标：现场生成 HICON 也失败了，标题栏将没有图标");
                    return;
                }
            }
        }
    };

    unsafe {
        // ICON_SMALL：标题栏那一档
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(hicon.0 as isize)),
        );
        // 类图标：新窗口 / WM_GETICON(ICON_SMALL2) 的兜底
        SetClassLongPtrW(hwnd, GCLP_HICONSM, hicon.0 as isize);
    }
    vlog!("窗口小图标已设置: {cx}x{cy}");
}

/// 从自己 exe 的图标资源里按目标尺寸取一个 HICON。
///
/// 资源 id 是 1（RT_GROUP_ICON），由 `icon::embed_into_exe` 写入 ——
/// 传的是组图标，`LoadImageW` 会从 7 档尺寸里挑最合适的一档。
#[cfg(windows)]
fn from_exe_resource(cx: i32, cy: i32) -> Option<HICON> {
    use windows::core::PCWSTR;

    // MAKEINTRESOURCE(1)：把 1 当成字符串指针（Win32 惯例）。
    #[allow(clippy::manual_dangling_ptr)]
    let name = PCWSTR(1 as *const u16);

    let module = unsafe { GetModuleHandleW(None) }.ok()?;
    let handle: HANDLE = unsafe {
        LoadImageW(
            Some(module.into()),
            name,
            IMAGE_ICON,
            cx,
            cy,
            // LR_SHARED：图标归系统管，不要（也不能）自己 DestroyIcon
            LR_DEFAULTCOLOR | LR_SHARED,
        )
    }
    .ok()?;
    Some(HICON(handle.0))
}

/// 备用：用 `src/icon.rs` 的绘制数据现场造一个 HICON。
///
/// ★ `CreateIcon` 要的是 **DIB** 格式：像素**自下而上**，
///   AND 掩码是 1bpp 且**每行按 WORD 对齐**（宽度不到 16 的倍数也要补齐）。
///   AND 位 = 1 表示"透明"。
#[cfg(windows)]
fn from_drawn_pixels(size: i32) -> Option<HICON> {
    use dufs_gui::icon;

    let n = size.max(1) as usize;
    let rgba = icon::draw(n as u32);

    // XOR：32bpp BGRA，自下而上
    let mut xor = Vec::with_capacity(n * n * 4);
    for y in (0..n).rev() {
        for x in 0..n {
            let i = (y * n + x) * 4;
            xor.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }

    // AND：1bpp，每行 16 位对齐，自下而上
    let row = n.div_ceil(16) * 2;
    let mut and = vec![0u8; row * n];
    for y in 0..n {
        let dst = (n - 1 - y) * row;
        for x in 0..n {
            if rgba[(y * n + x) * 4 + 3] == 0 {
                and[dst + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }

    unsafe { CreateIcon(None, size, size, 1, 32, and.as_ptr(), xor.as_ptr()).ok() }
}
