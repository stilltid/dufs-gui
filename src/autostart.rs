//! 开机自启（仅 Windows）
//!
//! 往**当前用户**的注册表写一项：
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`  → `dufs-gui = "<exe 路径>"`
//!
//! 三个刻意的选择：
//!
//! 1. **写 HKCU 而不是 HKLM** —— 不需要管理员权限，也不会影响别的用户。
//! 2. **只写路径，不带任何参数** —— 勾上只代表"登录后打开程序"，
//!    不代表"开机就启动服务"。悄悄开始分享文件是很糟糕的行为。
//! 3. **路径带引号** —— 程序放在带空格的目录里（`C:\Program Files\...` 或
//!    中文用户名）时不加引号会被 Windows 拆成两个参数，启动直接失败。
//!
//! ★ 注册表里存的是**当时那个 exe 的绝对路径**。程序换位置后这项就失效了
//!   （指向一个不存在的文件），界面上重新勾一次即可覆盖。

#[cfg(windows)]
mod imp {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey,
        RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };

    /// windows 0.62 的注册表 API 返回 `WIN32_ERROR`（就是个 u32 包装），不是 Result。
    /// 失败时把错误码带上，方便搜。
    fn check(r: WIN32_ERROR, what: &str) -> Result<(), String> {
        use crate::i18n::{K, tn};
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(tn(K::RegOpFailed, &[&what, &r.0]))
        }
    }

    /// 自启项的名字（任务管理器「启动」标签里显示的就是它）
    const VALUE_NAME: &str = "dufs-gui";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// 打开 Run 键（不创建）。返回 None 表示这条路走不通。
    fn open_run_key(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
        unsafe {
            let mut hkey = HKEY::default();
            let r = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                None,
                access,
                &mut hkey,
            );
            (r == ERROR_SUCCESS).then_some(hkey)
        }
    }

    /// 读出当前登记的命令行（没有这一项则返回 None）。
    fn read_value() -> Option<String> {
        let hkey = open_run_key(KEY_QUERY_VALUE)?;
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let mut ty = REG_SZ;
        let result = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(HSTRING::from(VALUE_NAME).as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut size),
            )
        };
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if result != ERROR_SUCCESS {
            return None;
        }
        // size 是字节数，含结尾 NUL
        let chars = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
    }

    /// 当前程序应该登记的完整命令行。
    ///
    /// `tray == true` 时多带一个 `--tray`：登录后启动的这一份直接静默进托盘，
    /// 不弹窗口。手动双击 exe 没有这个参数，所以永远会显示窗口 ——
    /// 这正是我们想要的（"双击了却什么都没出现"是最让人困惑的行为）。
    fn desired_value(tray: bool) -> Result<String, String> {
        let exe = std::env::current_exe()
            .map_err(|e| crate::i18n::tn(crate::i18n::K::AutoNoExePath, &[&e]))?;
        let s = exe.to_string_lossy().into_owned();
        if s.is_empty() {
            return Err(crate::i18n::t(crate::i18n::K::AutoEmptyExePath).to_string());
        }
        // ★ 必须带引号：路径里有空格时 Windows 会把一个参数拆成两个
        Ok(if tray {
            format!("\"{s}\" --tray")
        } else {
            format!("\"{s}\"")
        })
    }

    /// 从登记的命令行里拆出 exe 路径（去掉引号与后面的参数）。
    ///
    /// 只比路径、**不比后面的参数**：否则用户拨一下"静默启动"就会让
    /// `is_enabled()` 变成 false，界面莫名其妙显示成"未开启自启"。
    fn path_of(value: &str) -> Option<String> {
        let v = value.trim();
        if let Some(rest) = v.strip_prefix('"') {
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        } else {
            Some(v.split_whitespace().next()?.to_string())
        }
    }

    /// 登记的命令行里有没有 `--tray`
    pub fn has_tray_flag() -> bool {
        read_value()
            .map(|v| v.split_whitespace().any(|a| a == "--tray"))
            .unwrap_or(false)
    }

    /// 现在到底开没开。
    ///
    /// ★ 不只看"注册表里有没有这一项"，还要看它指的**是不是当前这个 exe** ——
    ///   程序换过位置时那一项还在，但它启动的是旧文件，这时候应该显示成"关"，
    ///   用户一勾就会用新路径覆盖掉。
    pub fn is_enabled() -> bool {
        let Some(value) = read_value() else {
            return false;
        };
        let Some(got) = path_of(&value) else {
            return false;
        };
        match std::env::current_exe() {
            Ok(exe) => got.eq_ignore_ascii_case(&exe.to_string_lossy()),
            Err(_) => false,
        }
    }

    /// 登记的那一项指向哪里（用于提示"指向了旧位置"）。
    pub fn registered_path() -> Option<String> {
        read_value()
    }

    pub fn set_enabled(on: bool, tray: bool) -> Result<(), String> {
        // 关：删掉这一项即可（不存在也算成功）
        if !on {
            let Some(hkey) = open_run_key(KEY_SET_VALUE) else {
                return Ok(()); // 键不存在 → 本来就没有自启项
            };
            let r = unsafe { RegDeleteValueW(hkey, &HSTRING::from(VALUE_NAME)) };
            unsafe {
                let _ = RegCloseKey(hkey);
            }
            // 本来就没有这一项 → 视为成功（关一个已经关着的东西不算失败）
            if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            return Err(crate::i18n::tn(
                crate::i18n::K::RegOpFailed,
                &[&crate::i18n::t(crate::i18n::K::RegOpDelete), &r.0],
            ));
        }

        // 开：需要可写权限；Run 键一般已存在，保险起见先试着打开，
        // 打不开再创建（RegCreateKeyExW 需要多一个 feature，这里不引入）
        let value = desired_value(tray)?;
        let hkey = open_run_key(KEY_SET_VALUE)
            .ok_or_else(|| crate::i18n::t(crate::i18n::K::AutoRunKeyLocked).to_string())?;

        // REG_SZ 要带结尾 NUL，按字节写入
        let mut bytes: Vec<u8> = Vec::new();
        for unit in value.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let r = unsafe {
            RegSetValueExW(
                hkey,
                &HSTRING::from(VALUE_NAME),
                None,
                REG_SZ,
                Some(&bytes),
            )
        };
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        check(r, crate::i18n::t(crate::i18n::K::RegOpWrite))
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_enabled() -> bool {
        false
    }
    pub fn registered_path() -> Option<String> {
        None
    }
    pub fn has_tray_flag() -> bool {
        false
    }
    pub fn set_enabled(_on: bool, _tray: bool) -> Result<(), String> {
        Err(crate::i18n::t(crate::i18n::K::AutoUnsupported).to_string())
    }
}

pub use imp::{has_tray_flag, is_enabled, registered_path, set_enabled};

#[cfg(all(test, windows))]
mod tests {
    use super::{has_tray_flag, is_enabled, registered_path, set_enabled};

    /// 真的写一遍注册表,看能不能读回来、能不能删干净。
    ///
    /// ★ 这个测试会**临时动用户的 HKCU\...\Run**,但出口只有两条,
    ///   而且都会把现场恢复成原样(原来是开的就写回去,原来没有就删掉):
    ///   测完用户看到的状态不变。
    #[test]
    fn roundtrip_registry() {
        let before = registered_path(); // None = 本来没这一项

        // 开:写进去
        set_enabled(true, false).expect("写自启项应当成功");
        assert!(is_enabled(), "写完之后应当读成“已开启”");
        let written = registered_path().expect("应当能读回自启项");
        assert!(
            written.starts_with('"'),
            "路径必须带引号（否则带空格的目录会被拆成两个参数）: {written}"
        );
        assert!(!has_tray_flag(), "没勾静默启动时不该带 --tray: {written}");

        // 勾上“静默启动”再写一次：同一条自启项，多一个 --tray
        set_enabled(true, true).expect("改写自启项应当成功");
        assert!(is_enabled(), "带参数的自启项照样算“已开启”");
        assert!(has_tray_flag(), "应当带上 --tray");
        set_enabled(true, false).expect("去掉 --tray 应当成功");
        assert!(!has_tray_flag(), "--tray 应当被去掉");
        let exe = std::env::current_exe().unwrap();
        assert!(
            written.to_lowercase().contains(&exe.to_string_lossy().to_lowercase()),
            "自启项应当指向当前 exe: {written}"
        );

        // 关:删干净
        set_enabled(false, false).expect("删自启项应当成功");
        assert!(!is_enabled(), "删完之后应当读成“未开启”");
        // 重复删一次不能报错（幂等）
        set_enabled(false, false).expect("重复删也应当算成功");

        // 恢复现场
        if let Some(old) = before {
            let _ = set_enabled(true, old.contains("--tray"));
        }
    }
}
