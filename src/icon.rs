//! 图标：纯代码生成 RGBA 像素，避免引入图片解码依赖、也不需要外部资源文件或画图工具。
//!
//! 本文件是图标的**唯一来源**：
//!   - 托盘图标（`app_icon_rgba`）用它；
//!   - 窗口/任务栏图标用它；
//!   - exe 文件图标（.ico）也由它栅格化而来；
//!   - 打包工具（`src/bin/icon_tool.rs`）的 `export <dir>` 导出预览图，用来在嵌入前先肉眼确认。
//!
//! 为什么坚持代码画：本机没有 `rc.exe`/`windres`/`llvm-rc`，
//! 也没有图片素材；代码画的东西可以随时改一个参数重新导出，且尺寸任意。

// 图标构图（定稿）：蓝底 + 描边窗口 + 白色文件夹。
//
// 全项目只有这一种构图 —— 托盘、窗口标题栏、任务栏、exe 文件图标
// （`icon.ico`）全吃同一份栅格化数据，不会出现「任务栏一个样、托盘另一个样」。

// 配色：蓝 #2D7FF9 + 白
const BLUE: [f64; 3] = [45.0, 127.0, 249.0];
const WHITE: [f64; 3] = [255.0, 255.0, 255.0];

/// 生成图标 RGBA 数据，返回 (rgba, width, height)。
///
/// 窗口标题栏（`main.rs` 里填 `app-icon`）、任务栏、托盘（`tray.rs`）都用这一份，
/// 且构图与 exe 文件图标（`icon.ico`）一致。
///
/// ★ 尺寸给 32x32 不够用：Slint 的 winit 后端把 window icon 按
///   `64 * scale_factor` 去要图（见 `i-slint-backend-winit/winitwindowadapter.rs`），
///   这里返回的多大就原样交给 `winit::window::Icon`（`render_to_buffer` 对
///   EmbeddedImage 不做缩放），高 DPI 下 32px 会被系统放大得发糊。
///   所以栅格化时就画大一点，让 Windows 自己挑合适的尺寸。
pub fn app_icon_rgba() -> (Vec<u8>, u32, u32) {
    const SIZE: u32 = 64;
    (draw(SIZE), SIZE, SIZE)
}

/// 栅格化：返回 size*size*4 的 RGBA（直通 alpha，未预乘）
///
/// ★ `size <= 32` 时用加粗简化版（描边提到约 3px，文件夹放大顶满）：
///   小尺寸档是“当 32px 设计”而不是“256px 缩下来”，和微信/VSCode 一个逻辑。
pub fn draw(size: u32) -> Vec<u8> {
    let n = size.max(1) as usize;
    let bold = size <= 32;
    // 小尺寸多采几倍（像素少，采密点边缘干净）；大尺寸 4x4 够了
    let ss: usize = if bold { 8 } else { 4 };
    let inv = 1.0 / n as f64;
    let total = (ss * ss) as f64;
    let mut out = vec![0u8; n * n * 4];

    for py in 0..n {
        for px in 0..n {
            let mut acc = [0.0f64; 4]; // 预乘累加
            for sy in 0..ss {
                for sx in 0..ss {
                    // 采样点归一化到 0..1
                    let x = (px as f64 + (sx as f64 + 0.5) / ss as f64) * inv;
                    let y = (py as f64 + (sy as f64 + 0.5) / ss as f64) * inv;
                    let (rgb, a) = sample(x, y, bold);
                    let af = a / 255.0;
                    acc[0] += rgb[0] * af;
                    acc[1] += rgb[1] * af;
                    acc[2] += rgb[2] * af;
                    acc[3] += a;
                }
            }
            let a = acc[3] / total;
            // 反预乘：acc[i] 是 Σ(rgb·a)，acc[3] 是 Σa，
            // 所以除以 acc[3] 才是按覆盖率加权的颜色平均值。
            // （★ 之前这里错写成 255.0/a，漏了样本数，RGB 被放大到 255 全变白）
            let (r, g, b) = if acc[3] > 1.0 {
                let k = 255.0 / acc[3];
                (acc[0] * k, acc[1] * k, acc[2] * k)
            } else {
                (0.0, 0.0, 0.0)
            };
            let i = (py * n + px) * 4;
            out[i] = r.clamp(0.0, 255.0) as u8;
            out[i + 1] = g.clamp(0.0, 255.0) as u8;
            out[i + 2] = b.clamp(0.0, 255.0) as u8;
            out[i + 3] = a.clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// 单个采样点：返回 (rgb, alpha)。坐标是 0..1 归一化的。
///
/// 所有形状都是不透明的，所以直接"后画的盖住先画的"，不需要做 alpha 混合。
fn sample(x: f64, y: f64, bold: bool) -> ([f64; 3], f64) {
    // 圆角蓝色底（圆角外透明）
    if !in_rrect(x, y, 0.0, 0.0, 1.0, 1.0, 0.22) {
        return ([0.0; 3], 0.0);
    }
    let mut c = BLUE;

    if bold {
        // 小尺寸版（≤32px）：线条按像素给足，文件夹顶满只留 1px 蓝缝
        if in_stroke_rrect(x, y, (0.14, 0.18, 0.86, 0.82, 0.08), 0.105) {
            c = WHITE;
        }
        if in_rrect(x, y, 0.14, 0.18, 0.86, 0.34, 0.08) {
            c = WHITE;
        }
        if in_folder_bold(x, y) {
            c = WHITE;
        }
    } else {
        // 大尺寸版：线条细一点，细节多一点
        if in_stroke_rrect(x, y, (0.16, 0.20, 0.84, 0.80, 0.07), 0.08) {
            c = WHITE;
        }
        if in_rrect(x, y, 0.16, 0.20, 0.84, 0.31, 0.07) {
            c = WHITE;
        }
        if in_folder(x, y) {
            c = WHITE;
        }
    }
    (c, 255.0)
}

/// 文件夹轮廓（小尺寸版）：顶满，只留 1px 蓝缝。
fn in_folder_bold(x: f64, y: f64) -> bool {
    // 标签
    if (0.28..=0.45).contains(&x) && (0.40..=0.48).contains(&y) {
        return true;
    }
    // 主体
    in_rrect(x, y, 0.28, 0.455, 0.72, 0.69, 0.035)
}

/// 文件夹轮廓：左上角一个小凸起（标签）+ 主体。仅用于判定“是否落在文件夹里”。
///
/// ★ 小尺寸版：整体比窗口描边内沿再缩一圈（左右 0.30~0.70、上下 0.41~0.66），
///   留出至少 2px 的蓝缝，否则 32px 下白文件夹会跟白描边连成一片。
fn in_folder(x: f64, y: f64) -> bool {
    // 标签
    if (0.30..=0.43).contains(&x) && (0.41..=0.47).contains(&y) {
        return true;
    }
    // 主体
    in_rrect(x, y, 0.30, 0.45, 0.70, 0.66, 0.03)
}

/// 圆角矩形内部
fn in_rrect(x: f64, y: f64, left: f64, top: f64, right: f64, bottom: f64, r: f64) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let cx = if x < left + r {
        left + r
    } else if x > right - r {
        right - r
    } else {
        return true;
    };
    let cy = if y < top + r {
        top + r
    } else if y > bottom - r {
        bottom - r
    } else {
        return true;
    };
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= r * r
}

/// 圆角矩形描边（外框内、内框外的部分）
fn in_stroke_rrect(x: f64, y: f64, rect: (f64, f64, f64, f64, f64), w: f64) -> bool {
    let (left, top, right, bottom, r) = rect;
    in_rrect(x, y, left, top, right, bottom, r)
        && !in_rrect(x, y, left + w, top + w, right - w, bottom - w, (r - w).max(0.0))
}

// ============================================================
// .ico 生成（供 build.rs 嵌入 exe）
// ============================================================

/// 图标里嵌哪些尺寸。
/// 16 = 任务栏/列表，32 = 任务栏大图标，48 = 资源管理器中等，
/// 256 = 大图标/桌面；64/128/24 用来补中间档，避免 Windows 拉伸发糊。
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// 把定稿构图写成多尺寸 `.ico`。
pub fn write_ico(
    path: &std::path::Path,
    sizes: &[u32],
) -> Result<(), String> {
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|s| (*s, dib(*s))).collect();

    let mut out: Vec<u8> = Vec::new();
    // ICONDIR
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    // ICONDIRENTRY[]
    let mut offset = 6 + 16 * images.len();
    for (size, data) in &images {
        // 256 要写成 0
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(dim);
        out.push(dim);
        out.push(0); // 调色板数
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in &images {
        out.extend_from_slice(data);
    }

    std::fs::write(path, out).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 单张图标的 DIB 数据（BITMAPINFOHEADER + 32bpp XOR 位图 + AND 掩码）。
///
/// ★ 两个容易写错的地方：
///   1. BITMAPINFOHEADER 里的高度要是**实际高度的两倍**（XOR 位图 + AND 掩码上下拼）；
///   2. 位图是**自下而上**存的。
fn dib(size: u32) -> Vec<u8> {
    let rgba = draw(size);
    let n = size as usize;

    // AND 掩码：每行 1bpp、按 4 字节对齐
    let mask_row = n.div_ceil(32) * 4;
    let and_size = mask_row * n;
    let xor_size = n * n * 4;

    let mut out: Vec<u8> = Vec::with_capacity(40 + xor_size + and_size);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(n as i32).to_le_bytes());
    out.extend_from_slice(&((n * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((xor_size + and_size) as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // XOR：BGRA，自下而上
    for y in (0..n).rev() {
        for x in 0..n {
            let i = (y * n + x) * 4;
            out.push(rgba[i + 2]);
            out.push(rgba[i + 1]);
            out.push(rgba[i]);
            out.push(rgba[i + 3]);
        }
    }
    // AND：32bpp 下全 0 即可（透明度由 alpha 决定）
    out.resize(out.len() + and_size, 0);
    out
}

// ============================================================
// 把图标写进 exe（用 Windows 资源 API，不依赖链接器）
// ============================================================

/// 把 `ico` 里的图标嵌进 `exe`（原地修改文件），返回嵌了几个尺寸。
///
/// ★ 为什么不用「`.rc` + `rc.exe`」或「链接器 `/RES:`」：
///   1. 本机没装 Windows SDK（也没有 `windres`，rustup 的 `llvm-tools` 里也没有 `llvm-rc`）；
///   2. 试过在 build.rs 里自己生成 `.res` 并加 `/RES:` 链接参数 —— 结果**悄无声息地没生效**。
///      原因是 PATH 里的 `link.exe` 是 **Git 自带的 coreutils `link`**（造硬链接的工具），
///      不是 MSVC 链接器，它把所有不认识参数全部忽略。
///      实测：连 `/NOSUCHLIB_XYZ.lib` 这种必定报错的参数都不报错。
///
///   所以改用微软文档化的资源 API：
///   `BeginUpdateResourceW` → `UpdateResourceW`（RT_ICON × N + RT_GROUP_ICON）→ `EndUpdateResourceW`。
///   它直接改 PE 文件，跟用哪个链接器无关，也不需要额外工具。
///
/// 注意：目标 exe **不能被运行中**（文件被占用时 BeginUpdateResourceW 会失败）。
pub fn embed_into_exe(exe: &std::path::Path, ico: &std::path::Path) -> Result<usize, String> {
    let entries = read_ico(ico)?;

    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::LibraryLoader::{
            BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
        };

        const RT_ICON: usize = 3;
        const RT_GROUP_ICON: usize = 14;
        const LANG_EN_US: u16 = 0x0409;

        /// 资源类型/名字传序数时的写法：把整数当成字符串指针（Win32 的 MAKEINTRESOURCE 惯例）。
        /// clippy 会误以为这是"悬垂指针"，其实是合法用法，所以在这里集中标注一次。
        fn ordinal(n: usize) -> PCWSTR {
            #[allow(clippy::manual_dangling_ptr)]
            let p = n as *const u16;
            PCWSTR(p)
        }

        let path: Vec<u16> = exe
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // false = 保留已有资源（比如清单）
        let update: HANDLE = unsafe { BeginUpdateResourceW(PCWSTR(path.as_ptr()), false) }
            .map_err(|e| format!("无法打开 {} 更新资源（文件是不是正在运行？）: {e}", exe.display()))?;

        // 内部小工具：出错时要把 EndUpdateResourceW(discard=true) 调上，否则可能留下半个文件
        let mut fail: Option<String> = None;
        for (i, bytes) in entries.iter().enumerate() {
            let id = i + 1;
            let r = unsafe {
                UpdateResourceW(
                    update,
                    ordinal(RT_ICON),
                    ordinal(id),
                    LANG_EN_US,
                    Some(bytes.as_ptr() as *const core::ffi::c_void),
                    bytes.len() as u32,
                )
            };
            if let Err(e) = r {
                fail = Some(format!("写入图标资源 {id} 失败: {e}"));
                break;
            }
        }

        if fail.is_none() {
            let grp = group_icon_data(&entries);
            let r = unsafe {
                UpdateResourceW(
                    update,
                    ordinal(RT_GROUP_ICON),
                    ordinal(1),
                    LANG_EN_US,
                    Some(grp.as_ptr() as *const core::ffi::c_void),
                    grp.len() as u32,
                )
            };
            if let Err(e) = r {
                fail = Some(format!("写入图标组失败: {e}"));
            }
        }

        let discard = fail.is_some();
        let end = unsafe { EndUpdateResourceW(update, discard) };
        if let Some(msg) = fail {
            let _ = end;
            return Err(msg);
        }
        end.map_err(|e| format!("提交资源更新失败: {e}"))?;

        Ok(entries.len())
    }

    #[cfg(not(windows))]
    {
        let _ = (exe, entries);
        Err("只有 Windows 需要往 exe 里写图标".into())
    }
}

/// 读一个 .ico，返回里面每张图的原始字节（尺寸从各自的 DIB 头里读）
fn read_ico(path: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    let buf = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    parse_ico(&buf)
}

fn parse_ico(buf: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    if buf.len() < 6 {
        return Err("ico 文件太小".into());
    }
    let count = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let o = 6 + i * 16;
        if o + 16 > buf.len() {
            return Err("ico 目录项越界".into());
        }
        let size = u32::from_le_bytes([buf[o + 8], buf[o + 9], buf[o + 10], buf[o + 11]]) as usize;
        let off = u32::from_le_bytes([buf[o + 12], buf[o + 13], buf[o + 14], buf[o + 15]]) as usize;
        if off + size > buf.len() {
            return Err("ico 图像数据越界".into());
        }
        out.push(buf[off..off + size].to_vec());
    }
    if out.is_empty() {
        return Err("ico 里没有任何图像".into());
    }
    Ok(out)
}

/// GRPICONDIR：告诉 Windows 这个图标组里有哪些尺寸、分别对应哪个 RT_ICON 编号
fn group_icon_data(images: &[Vec<u8>]) -> Vec<u8> {
    // 这里只需要宽/高/bpp，从 ico 目录里已经拿过了；为了不重复解析，
    // 直接从 DIB 头里读回来（BITMAPINFOHEADER 前 16 字节就有宽高和 bpp）。
    let mut out = Vec::with_capacity(6 + images.len() * 14);
    out.extend_from_slice(&0u16.to_le_bytes()); // idReserved
    out.extend_from_slice(&1u16.to_le_bytes()); // idType = 1（图标）
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, img) in images.iter().enumerate() {
        let (w, h, bpp) = dib_dims(img).unwrap_or((0, 0, 32));
        out.push(w);
        out.push(h);
        out.push(0); // 调色板数
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&bpp.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&((i + 1) as u16).to_le_bytes()); // 对应哪个 RT_ICON
    }
    out
}

/// 从 DIB 头里读出 (宽, 高, bpp)，都按 ico 规范转成单字节形式（256 -> 0）
fn dib_dims(img: &[u8]) -> Option<(u8, u8, u16)> {
    if img.len() < 16 {
        return None;
    }
    let w = i32::from_le_bytes([img[4], img[5], img[6], img[7]]).unsigned_abs();
    let h = i32::from_le_bytes([img[8], img[9], img[10], img[11]]).unsigned_abs() / 2; // 高度是两倍
    let bpp = u16::from_le_bytes([img[14], img[15]]);
    Some((dim_byte(w), dim_byte(h), bpp))
}

fn dim_byte(v: u32) -> u8 {
    if v >= 256 {
        0 // ico 规范：256 写成 0
    } else {
        v as u8
    }
}

// ============================================================
// 预览导出（开发用）
// ============================================================

/// 把定稿构图导出成一张“尺寸对比图”（BMP，便于直接看图确认）。
///
/// 里面按顺序摆：256px、48px、32px、16px、以及 16px 放大 8 倍（最近邻，用来看小尺寸下的真实像素）。
/// 图形用透明背景，导出时压在中灰底上，方便看圆角与边缘。
pub fn export_previews(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let mut written = Vec::new();

    // 定稿的构图也导出一份 .ico，编译时由 build.rs 嵌进 exe 的图标资源。
    // ★ 和托盘/窗口那份吃的是同一个 `draw()`，天然同步。
    //   改了绘制代码后重跑本命令，把 icon.ico 覆盖到 assets/ 即可。
    let ico = dir.join("icon.ico");
    write_ico(&ico, &ICO_SIZES)?;
    written.push(ico);

    let path = dir.join("preview.bmp");
    write_preview_bmp(&path)?;
    written.push(path);
    Ok(written)
}

fn write_preview_bmp(path: &std::path::Path) -> Result<(), String> {
    const BG: [u8; 3] = [243, 244, 246]; // 中灰底
    const GAP: usize = 18;
    const PAD: usize = 16;

    let big = (256u32, draw(256));
    let s48 = (48u32, draw(48));
    let s32 = (32u32, draw(32));
    let s16 = (16u32, draw(16));
    let zoom = 8usize; // 16px 放大 8 倍
    let zoomed = (16 * zoom, s16.1.clone());

    let width = PAD * 2 + 256 + GAP + 48 + GAP + 32 + GAP + 16 + GAP + 16 * zoom;
    let height = PAD * 2 + 256;
    // RGB 平铺（不能用 vec![[u8;3]]，下面按下标平铺写更方便）
    let mut canvas = vec![0u8; width * height * 3];
    for chunk in canvas.chunks_exact_mut(3) {
        chunk.copy_from_slice(&BG);
    }

    // 把每个尺寸贴上去（垂直居中）
    let place = |canvas: &mut Vec<u8>, x0: usize, size_px: usize, scale: usize, rgba: &[u8]| {
        let y0 = PAD + (256 - size_px * scale) / 2;
        for py in 0..size_px * scale {
            for px in 0..size_px * scale {
                // scale>1 时用最近邻，保留真实像素感
                let sx = px / scale;
                let sy = py / scale;
                let i = (sy * size_px + sx) * 4;
                let a = rgba[i + 3] as f64 / 255.0;
                if a <= 0.004 {
                    continue;
                }
                let (cx, cy) = (x0 + px, y0 + py);
                if cx >= width || cy >= height {
                    continue;
                }
                let o = (cy * width + cx) * 3;
                for k in 0..3 {
                    canvas[o + k] = (rgba[i + k] as f64 * a + canvas[o + k] as f64 * (1.0 - a)) as u8;
                }
            }
        }
    };

    let mut x = PAD;
    place(&mut canvas, x, big.0 as usize, 1, &big.1);
    x += 256 + GAP;
    place(&mut canvas, x, s48.0 as usize, 1, &s48.1);
    x += 48 + GAP;
    place(&mut canvas, x, s32.0 as usize, 1, &s32.1);
    x += 32 + GAP;
    place(&mut canvas, x, s16.0 as usize, 1, &s16.1);
    x += 16 + GAP;
    place(&mut canvas, x, 16, zoom, &zoomed.1);

    write_bmp24(path, &canvas, width, height)
}

/// 24 位 BMP（BMP 的行是自下而上、BGR、行宽 4 字节对齐）
fn write_bmp24(path: &std::path::Path, rgb: &[u8], width: usize, height: usize) -> Result<(), String> {
    let row_raw = width * 3;
    let padding = (4 - row_raw % 4) % 4;
    let row_size = row_raw + padding;
    let pixel_bytes = row_size * height;
    let file_size = 54 + pixel_bytes;

    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    for y in (0..height).rev() {
        for x in 0..width {
            let i = (y * width + x) * 3;
            out.push(rgb[i + 2]); // B
            out.push(rgb[i + 1]); // G
            out.push(rgb[i]); // R
        }
        out.resize(out.len() + padding, 0);
    }

    std::fs::write(path, out).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

// ============================================================
// 测试：钉死「同一个图标」这件事
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 .ico 里取出某个尺寸，还原成自上而下的 RGBA。
    fn ico_rgba_at(ico: &[u8], size: u32) -> Vec<u8> {
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        assert!(count > 0, "ico 里一个尺寸都没有");
        for i in 0..count {
            let e = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            let dim = e[0] as u32; // 0 表示 256
            let real = if dim == 0 { 256 } else { dim };
            if real != size {
                continue;
            }
            let bytes = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
            let off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
            let dib = &ico[off..off + bytes];
            let h = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]) / 2;
            assert_eq!(h as u32, size, "DIB 高度必须是实际高度的两倍");
            let n = size as usize;
            // 像素区在 40 字节的 BITMAPINFOHEADER 之后，自下而上存 BGRA
            let px = &dib[40..40 + n * n * 4];
            let mut out = vec![0u8; n * n * 4];
            for y in 0..n {
                let src = (n - 1 - y) * n * 4;
                let dst = y * n * 4;
                for x in 0..n {
                    out[dst + x * 4] = px[src + x * 4 + 2];
                    out[dst + x * 4 + 1] = px[src + x * 4 + 1];
                    out[dst + x * 4 + 2] = px[src + x * 4];
                    out[dst + x * 4 + 3] = px[src + x * 4 + 3];
                }
            }
            return out;
        }
        panic!("ico 里没有 {size}px 这一档");
    }

    /// ★ 这条就是防「任务栏一个样、托盘另一个样」复发的。
    ///
    /// `assets/icon.ico` 是嵌进 exe 的文件图标（任务栏/资源管理器看到的），
    /// `app_icon_rgba()` 是窗口标题栏 + 托盘看到的。两者必须逐像素相同。
    #[test]
    fn exe_图标与运行时图标一致() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.ico");
        let ico = std::fs::read(path)
            .unwrap_or_else(|e| panic!("读不到 {path}: {e}（先跑 cargo run --bin icon_tool -- export）"));

        let (rgba, w, _h) = app_icon_rgba();
        let expected = draw(w);
        assert_eq!(rgba, expected, "app_icon_rgba 必须是 draw() 画出来的");

        // 小尺寸是真会被用到的（标题栏 16px、任务栏 32px），拿 32 比最有代表性。
        let from_ico = ico_rgba_at(&ico, 32);
        assert_eq!(
            from_ico,
            draw(32),
            "assets/icon.ico 的 32px 档和 draw() 不一致：\
             改了构图后要重跑 `icon_tool export` 并覆盖 assets/icon.ico"
        );
    }

    /// 标题栏那档（16px）也得在 ico 里，否则 Windows 会拉伸 32px 的图。
    #[test]
    fn ico_尺寸齐全() {
        let ico = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.ico")).unwrap();
        for s in ICO_SIZES {
            let _ = ico_rgba_at(&ico, s); // 缺了会 panic
        }
    }
}
