//! 原生文件拖放（Windows）。
//!
//! 背景：Slint 自带的 `DropArea` **收不到**资源管理器拖进来的文件。
//! 查过 Slint 1.18 的源码：winit 后端在分发 winit 事件时，
//! `DroppedFile` / `HoveredFile` 走的是 `_ => {}`（直接丢掉），
//! 而 Slint 的原生拖放（`process_drag_event`）只有 testing 后端实现了。
//! 所以这里绕开 Slint，自己跟 Windows 要拖放。
//!
//! 做法：
//!   1. 先 `RevokeDragDrop` 撤掉 winit 默认注册的那个 `IDropTarget`
//!      （反正 Slint 不接它的事件；不撤的话我们自己的 `RegisterDragDrop`
//!      会报「已经注册过」）；
//!   2. 注册我们自己的 `IDropTarget`（下面的 `DropTarget`）；
//!   3. 拖进来的是文件（CF_HDROP）才接受，否则显示「禁止」光标；
//!   4. 松手时把路径交给调用方，界面只负责显示（高亮、提示）。
//!
//! 线程：OLE 回调跑在窗口线程（实践中就是 UI 线程）上，但**不依赖这一点** ——
//! 传进来的两个回调可能在任意线程被调，调用方必须只用线程安全的操作
//! （`ui::ui_set` / `set_hint` / `Mutex`），不要直接写 Slint 属性。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{implement, Ref};
use windows::Win32::Foundation::{HWND, POINTL, S_OK};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

// 注册成功后**不需要**自己留一份 interface 保活。
//
// `RegisterDragDrop` 会 AddRef，OLE 拿着那个引用直到 `RevokeDragDrop`
// （进程退出时 OS 会清理；winit 在窗口销毁时也会调一次 `RevokeDragDrop`，
// 顺手把我们的也撤了，正好）。这里的 `IDropTarget` 值用完即 drop，
// 只释放我们自己手里的那一个引用 —— 不能放进 `static` 里，
// 因为 COM interface 的指针不是 `Send + Sync`。

/// 调用方提供的两个回调。`Send` 即可，不要求 `Sync` ——
/// 访问永远经过外面的 `Mutex`，不会出现 `&Sink` 跨线程共享。
struct Sink {
    /// 有文件悬停 / 离开时调用（`true` = 正在悬停，界面据此高亮）。
    on_hover: Box<dyn Fn(bool) + Send>,
    /// 用户松手、且拖的是文件时调用，参数是原始路径列表。
    /// 「只能一个」等判断由调用方做（`config::decide_dropped_path`）。
    on_paths: Box<dyn Fn(Vec<PathBuf>) + Send>,
}

/// 我们自己的 OLE 拖放目标。
///
/// ★ 字段只能放 `Send + Sync` 的东西（COM 对象会被跨线程引用）：
///   `Arc<Mutex<Sink>>` + `AtomicU32` 满足，`slint::Weak` 不直接放这里，
///   藏进调用方的回调里（回调只要求 `Send`）。
#[implement(IDropTarget)]
struct DropTarget {
    sink: Arc<Mutex<Sink>>,
    /// `DragEnter` 里谈好的效果。`DragOver` 拿不到 data object，
    /// 只能把这里存的值原样返回。
    effect: AtomicU32,
}

/// CF_HDROP 的查询条件（`DragEnter` 用只问不取的 `QueryGetData`，
/// `Drop` 用真的 `GetData`）。
fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// 对方是否在提供文件列表（只问，不取）。
fn has_files(data: &IDataObject) -> bool {
    let fmt = hdrop_format();
    unsafe { data.QueryGetData(&fmt) == S_OK }
}

/// 把 CF_HDROP 读成路径列表。拿不到 / 不是 HGLOBAL / 空指针都返回 None。
///
/// ★ 拿到 `STGMEDIUM` 就拥有了它，出口只有两条：
///   成功读完 → `ReleaseStgMedium`；中途发现不对 → 同样 `ReleaseStgMedium`。
///   漏掉就是 GDI 句柄泄漏（拖一次漏一次）。
fn hdrop_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let fmt = hdrop_format();
    let mut medium: STGMEDIUM = unsafe {
        match data.GetData(&fmt) {
            Ok(m) => m,
            // 最常见的是 DV_E_FORMATETC：拖的不是文件（比如一段文字）。
            // 这是正常情况，不是错误，不用记日志。
            Err(_) => return None,
        }
    };

    // 拿 CF_HDROP 却给了别的 tymed（理论上不会）：放掉走人，别碰 union。
    if medium.tymed != TYMED_HGLOBAL.0 as u32 {
        unsafe { ReleaseStgMedium(&mut medium) };
        return None;
    }
    // SAFETY: 上面刚验证过 tymed 是 HGLOBAL，读 union 的这个分支是合法的。
    let hdrop = HDROP(unsafe { medium.u.hGlobal.0 });
    if hdrop.0.is_null() {
        unsafe { ReleaseStgMedium(&mut medium) };
        return None;
    }

    // iFile = 0xFFFFFFFF 表示「只要个数」；再逐个取长度、取值。
    // （winit 自己的 drop_handler 也是这个套路。）
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        // +1 留给结尾的 NUL；按实际长度切片，NUL 不会进字符串。
        let mut buf = vec![0u16; len + 1];
        unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
        out.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
    }

    unsafe { ReleaseStgMedium(&mut medium) };
    Some(out)
}

/// ★ 方法实现在宏生成的 `DropTarget_Impl` 上（不是 `DropTarget` 本身）。
///   `DropTarget_Impl` 会 `Deref` 到 `DropTarget`，所以方法里照常用
///   `self.sink` / `self.effect`。这是 `#[implement]` 的固定套路。
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Ref<IDataObject>,
        _key: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let accept = pdataobj.as_ref().is_some_and(has_files);
        let fx = if accept { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        self.effect.store(fx.0, Ordering::SeqCst);
        // SAFETY: OLE 保证这个指针在回调期间有效。
        unsafe { *effect = fx };
        if let Ok(sink) = self.sink.lock() {
            (sink.on_hover)(accept);
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _key: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let fx = DROPEFFECT(self.effect.load(Ordering::SeqCst));
        // SAFETY: 同上。
        unsafe { *effect = fx };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.effect.store(DROPEFFECT_NONE.0, Ordering::SeqCst);
        if let Ok(sink) = self.sink.lock() {
            (sink.on_hover)(false);
        }
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Ref<IDataObject>,
        _key: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let mut fx = DROPEFFECT_NONE;
        if let Some(data) = pdataobj.as_ref()
            && let Some(paths) = hdrop_paths(data)
            && !paths.is_empty()
        {
            fx = DROPEFFECT_COPY;
            if let Ok(sink) = self.sink.lock() {
                (sink.on_paths)(paths);
            }
        }
        self.effect.store(DROPEFFECT_NONE.0, Ordering::SeqCst);
        // 松手后高亮必须灭掉（DragLeave 不一定会来，不能指望它）。
        if let Ok(sink) = self.sink.lock() {
            (sink.on_hover)(false);
        }
        // SAFETY: 同上。
        unsafe { *effect = fx };
        Ok(())
    }
}

/// 在主窗口上启用文件拖放。
///
/// 找窗口走 `window_finder::main_hwnd()`（**按 PID**；和托盘 / 文件对话框的
/// parent 是同一套机制）。
/// ★ **不按标题找** —— 标题是要翻译的界面文案，按标题找的话英文界面下
///   这里会静默失败，表现是「拖文件进窗口没反应」。
/// `on_hover` / `on_paths` 可能在任意线程被调（实践中是 UI 线程），
/// 里面只许用线程安全的操作。
/// ★ 它们是在 OLE 回调里**同步**执行的，所以不能做阻塞操作
///   （重活请自己丢后台线程，参考 `main.rs::install_drop_target`）。
///
/// 失败返回中文原因，调用方记日志即可 —— 拖拽只是锦上添花，
/// 坏了不该影响主流程，用户仍能用「选择」按钮。
pub fn install(
    on_hover: impl Fn(bool) + Send + 'static,
    on_paths: impl Fn(Vec<PathBuf>) + Send + 'static,
) -> Result<(), String> {
    let raw = crate::window_finder::main_hwnd()
        .ok_or_else(|| "找不到主窗口，拖拽功能未能启用".to_string())?;
    let hwnd = HWND(raw as *mut _);

    // 先撤掉 winit 默认注册的那个。不撤的话下面这句会报「已注册过」。
    // 失败也无所谓（可能本来就没注册上）。
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }

    let target: IDropTarget = DropTarget {
        sink: Arc::new(Mutex::new(Sink {
            on_hover: Box::new(on_hover),
            on_paths: Box::new(on_paths),
        })),
        effect: AtomicU32::new(DROPEFFECT_NONE.0),
    }
    .into();

    unsafe { RegisterDragDrop(hwnd, &target) }
        .map_err(|e| format!("注册拖放目标失败（{e}），拖拽功能不可用"))?;

    // ★ `target` 在这里 drop，只释放我们手里的引用，OLE 那边还拿着一个，
    //   对象会一直活到 RevokeDragDrop / 进程退出。不要试图 static 留存
    //   —— IDropTarget 不是 Send + Sync，放不进 static。
    drop(target);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! 确定性单测：用 Shell 真实的数据对象（和资源管理器拖出来的是同一种），
    //! 不用窗口、不用鼠标。跑 `cargo test --bin dufs-gui`。
    use super::{has_files, hdrop_paths};
    use windows::core::HSTRING;
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, IShellItem, SHCreateItemFromParsingName,
    };

    /// 给某个真实文件造一个 Shell 的 `IDataObject`（`BHID_DataObject`）。
    /// 资源管理器拖文件时，交出来的就是这东西。
    fn shell_data_object(path: &std::path::Path) -> IDataObject {
        unsafe {
            // Shell 调用前先 OleInitialize，否则 BindToHandler 可能失败。
            let _ = OleInitialize(None);
            let item: IShellItem =
                SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)
                    .expect("parse path");
            item.BindToHandler(None, &BHID_DataObject)
                .expect("data object")
        }
    }

    /// 把路径规范化成「绝对 + 长名」形式再比较。
    ///
    /// ★ 为什么不能直接比字符串：GitHub Actions 的 Windows runner 上 `%TEMP%`
    ///   是 **8.3 短名**（`C:\Users\RUNNER~1\AppData\Local\Temp`），而 Shell 的
    ///   `CF_HDROP` 交回来的是**长名**（`C:\Users\runneradmin\...`）。同一个文件、
    ///   两种拼写，直接 `assert_eq!` 必挂 —— 而且 CI 上是**必现**的，不是偶发。
    ///   本地 `%TEMP%` 本来就是长名，所以本地永远复现不出来（曾经就是这么红的）。
    ///
    /// 两边都过一遍 `canonicalize`：Windows 上它会带上 `\\?\` 前缀，只要
    /// 两边都规范化，前缀也一样，比较才有意义。文件由调用方保证存在。
    fn canonical(p: &std::path::Path) -> std::path::PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|e| panic!("规范化路径失败 {}: {e}", p.display()))
    }

    #[test]
    fn shell_hdrop_roundtrip() {
        let file = std::env::temp_dir().join("dufs-gui-drop-probe.txt");
        std::fs::write(&file, b"probe").unwrap();

        let data = shell_data_object(&file);
        assert!(has_files(&data), "CF_HDROP 应当被识别为文件拖放");
        let paths = hdrop_paths(&data).expect("应当读出路径列表");
        assert_eq!(
            paths.iter().map(|p| canonical(p)).collect::<Vec<_>>(),
            vec![canonical(&file)],
            "读出的路径应当和拖进去的一致（按规范化后的路径比较，见 canonical 的注释）"
        );

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn shell_hdrop_probes_stgmedium_release() {
        // 同一个文件反复读 50 次：如果 `ReleaseStgMedium` 漏调，
        // 句柄泄漏会在这里累积（用任务管理器看 GDI 对象会涨）。
        // 这里只保证不崩、结果稳定；泄漏靠代码审查 + 手动观察。
        let file = std::env::temp_dir().join("dufs-gui-drop-probe2.txt");
        std::fs::write(&file, b"probe").unwrap();
        let data = shell_data_object(&file);
        let want = vec![canonical(&file)];
        for _ in 0..50 {
            let paths = hdrop_paths(&data).expect("反复读应当都成功");
            assert_eq!(
                paths.iter().map(|p| canonical(p)).collect::<Vec<_>>(),
                want,
                "反复读出的路径同样要按规范化后的形式比较"
            );
        }
        let _ = std::fs::remove_file(&file);
    }
}
