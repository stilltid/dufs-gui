//! 二维码生成：URL 字符串 -> Slint 可直接显示的 Image
//!
//! 这里直接操作像素缓冲，不依赖 `image` crate：
//! `qrcode` 的 `to_vec()` 已经给出模块位图，我们只需要把它按整数倍放大
//! 并填进 RGBA 缓冲即可（省掉一次编解码，也省掉一个依赖）。

use qrcode::types::Color;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// 每个二维码模块放大成多少像素（手机上 4 已经很清晰）
const SCALE: usize = 5;
/// 静默区（quiet zone）宽度，单位：模块。
/// ★ 标准要求至少 4 个模块，少了会导致部分手机相机识别失败。
const QUIET: usize = 4;

/// 生成二维码图片。失败时（数据过长等）返回 None，调用方决定怎么提示。
pub fn make_image(text: &str) -> Option<Image> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;

    let modules = code.width();
    // 模块颜色（Dark = 深色）
    let colors = code.to_colors();

    let side_modules = modules + QUIET * 2;
    let side = side_modules * SCALE;

    let mut buf = vec![0u8; side * side * 4]; // RGBA

    for y in 0..side {
        // 映射回"模块坐标"，落在静默区就是白
        let my = y / SCALE;
        let in_y = my >= QUIET && my < QUIET + modules;

        for x in 0..side {
            let mx = x / SCALE;
            let in_x = mx >= QUIET && mx < QUIET + modules;

            let dark = in_x && in_y && colors[(my - QUIET) * modules + (mx - QUIET)] == Color::Dark;
            let v: u8 = if dark { 0 } else { 255 };

            let i = (y * side + x) * 4;
            buf[i] = v;     // R
            buf[i + 1] = v; // G
            buf[i + 2] = v; // B
            buf[i + 3] = 255; // A
        }
    }

    let pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&buf, side as u32, side as u32);
    Some(Image::from_rgba8(pixel_buffer))
}
