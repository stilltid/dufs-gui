//! UI 线程小工具：跨线程写 Slint 属性的唯一合法通道。
//!
//! ★ Slint 的属性只能由 UI 线程修改。后台线程（选文件、扫网卡、
//!   netsh、托盘、拖放）一律走 `ui_set()` 把活投进事件循环，
//!   由 UI 线程下一帧执行。`set_hint / set_status / set_busy` 是高频单功能的糖。

use crate::MainWindow;
use slint::{ComponentHandle, SharedString, Weak};

// ============================================================
// 界面写入小工具
// ============================================================

/// 统一切回 UI 线程写属性。
///
/// ★ 两条硬规则：
///   1. Slint 的属性只能由 UI 线程修改，一切界面写入必须走这里。
///   2. `slint::Image` **不是 Send**，所以图片必须在闭包内部现生成，
///      不能在子线程里做好再传进来。
pub fn ui_set(ui: &Weak<MainWindow>, f: impl FnOnce(&MainWindow) + Send + 'static) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            f(&ui);
        }
    });
}

pub fn set_hint(ui: &Weak<MainWindow>, text: impl Into<SharedString>, is_error: bool) {
    let text = text.into();
    ui_set(ui, move |ui| {
        ui.set_hint_text(text);
        ui.set_hint_is_error(is_error);
    });
}

pub fn set_status(ui: &Weak<MainWindow>, text: impl Into<SharedString>) {
    let text = text.into();
    ui_set(ui, move |ui| ui.set_status_text(text));
}

pub fn set_busy(ui: &Weak<MainWindow>, busy: bool) {
    ui_set(ui, move |ui| ui.set_busy(busy));
}

// ============================================================
// 窗口尺寸
// ============================================================

/// 默认宽度 / 未运行时高度 / 运行时高度（逻辑像素）
///
/// ★ 这些值**只能**用于状态切换时的 `set_size`，
///   绝不能写回 `ui/app.slint` 的 `width` / `height`：
///   Slint 用 `window_is_resizable(min, max)` 判定能否缩放，
///   一旦 min 和 max 相等（写死尺寸或写成 `running ? a : b` 这类绑定），
///   就会 `set_resizable(false)` —— 后果是 WS_THICKFRAME 被移除、
///   `WindowButtons::MAXIMIZE` 被关掉，用户拖不动边框、最大化按钮变灰。
/// 窗口宽度
///
/// 试探过改成「左侧导航 + 内容区」(左栏 150px + 间距 12px 要吃掉 190px),
/// 但那样内容区在 620 宽时只剩 410px,一行控件放不下,最终仍是顶部分页头,
/// 所以宽度保持原值。
pub const WINDOW_WIDTH: f32 = 600.0;
pub const IDLE_HEIGHT: f32 = 620.0;
pub const RUNNING_HEIGHT: f32 = 880.0;

/// 运行中比未运行多出来的高度 = 「手机访问」（二维码）那一块。
///
/// ★ 所有"运行时变高"的地方都用它算，而不是直接写死 `RUNNING_HEIGHT` ——
///   这样用户自己拖出来的高度也能被尊重：最终高度 = 用户基准 + 这个差值。
pub const QR_EXTRA_HEIGHT: f32 = RUNNING_HEIGHT - IDLE_HEIGHT;

/// 基准高度的下限，防止存出/算出又矮又挤的窗口
pub const MIN_BASE_HEIGHT: f32 = 200.0;

/// 由「当前窗口高度 + 当前是否在运行」反推出**未运行口径的基准高度**。
///
/// 这是整套高度逻辑的**唯一一处换算**：`fit_window` 和退出时的存盘都调它，
/// 两侧口径不可能对不上（以前一处减、一处不减，就会每次启停都漂移）。
pub fn base_height_from(current_h: f32, was_running: bool) -> f32 {
    let base = if was_running {
        current_h - QR_EXTRA_HEIGHT
    } else {
        current_h
    };
    base.max(MIN_BASE_HEIGHT)
}

/// 按运行状态调整窗口大小。
///
/// ★ **调用方必须先调它、再改 `ui.running`** —— 它靠 `ui.get_running()`
///   判断当前高度是哪种口径（有没有含二维码那一块）。顺序反了，用户刚
///   拖出来的高度就会被当成另一种口径，表现就是"一启停就跳回默认高度"。
///
/// 高度算法：先把当前高度换算成**基准值**（"未运行高度"），再加新状态需要的块。
/// 所以用户随手拖多高都能被尊重，而不是每次都被掰回硬编码值。
///
/// `fallback_width` / `fallback_base_height`：窗口还没创建（启动早期）时
/// `size()` 是 0，这时用上次存下来的值，没存过再用设计默认值。
pub fn fit_window(
    ui: &MainWindow,
    running: bool,
    fallback_width: f32,
    fallback_base_height: Option<f32>,
) {
    let window = ui.window();
    if window.is_maximized() || window.is_minimized() {
        return;
    }

    let scale = window.scale_factor();
    let current = window.size(); // PhysicalSize
    let width = if current.width > 0 {
        current.width as f32 / scale
    } else {
        fallback_width
    };

    let was_running = ui.get_running();
    let base = if current.height > 0 {
        base_height_from(current.height as f32 / scale, was_running)
    } else {
        fallback_base_height.unwrap_or(IDLE_HEIGHT).max(MIN_BASE_HEIGHT)
    };

    let height = if running { base + QR_EXTRA_HEIGHT } else { base };

    // set_size 返回 ()，窗口还没创建时会内部记成待应用属性
    window.set_size(slint::WindowSize::Logical(slint::LogicalSize::new(
        width, height,
    )));
}

#[cfg(test)]
mod tests {
    use super::{base_height_from, IDLE_HEIGHT, MIN_BASE_HEIGHT, QR_EXTRA_HEIGHT};

    /// 启停来回切不能越切越高/越切越矮（以前一边减一边不减，就会漂）
    #[test]
    fn 高度基准_启停往返不漂移() {
        // 用户拖到 1000 高，此时在运行
        let base = base_height_from(1000.0, true);
        assert_eq!(base, 1000.0 - QR_EXTRA_HEIGHT);
        // 停止服务 → 高度就是基准；再启动 → 又回到 1000
        let idle_h = base;
        assert_eq!(base_height_from(idle_h, false), base);
        assert_eq!(base + QR_EXTRA_HEIGHT, 1000.0);
    }

    #[test]
    fn 高度基准_按当前状态解读当前高度() {
        assert_eq!(base_height_from(IDLE_HEIGHT, false), IDLE_HEIGHT);
        assert_eq!(base_height_from(880.0, true), 620.0);
        // 拖矮了也不会算出小于下限的基准
        assert_eq!(base_height_from(50.0, false), MIN_BASE_HEIGHT);
        assert_eq!(base_height_from(100.0, true), MIN_BASE_HEIGHT);
    }

    /// 常量的阳断：两个设计高度之差就是二维码那一块，改一个忘改另一个会挂在编译期
    #[test]
    fn 二维码块高度等于两个设计高度之差() {
        assert_eq!(QR_EXTRA_HEIGHT, super::RUNNING_HEIGHT - super::IDLE_HEIGHT);
    }
}

