//! 系统环境变量管理：读取 / 新增 / 修改 / 删除用户（HKCU\Environment）与
//! 系统（HKLM\…\Session Manager\Environment）两级注册表变量。
//!
//! 写入后通过 `WM_SETTINGCHANGE` 广播通知已运行的资源管理器和新进程，
//! 等价于「编辑系统环境变量」对话框的行为；已启动的进程（含本应用自身）
//! 不会热更新，需要重启才能拿到新值。

use serde::Serialize;

/// 受保护变量：删除会破坏系统基本功能（PATH 丢失后命令行工具全部失联），
/// 删除时后端直接拒绝，不做「确认了也照删」的危险放行。
const PROTECTED_NAMES: [&str; 6] = ["path", "pathext", "windir", "systemroot", "systemdrive", "comspec"];

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EnvVar {
    pub name: String,
    /// 注册表里的原始存储值：REG_EXPAND_SZ 保留 `%…%` 引用原样展示。
    pub value: String,
    /// 原始值按当前环境展开后的效果预览；与 value 相同时为 None。
    pub expanded: Option<String>,
    /// 是否以 REG_EXPAND_SZ（含 %引用%）存储。
    pub is_expand: bool,
}

#[cfg(windows)]
mod imp {
    use super::{EnvVar, PROTECTED_NAMES};
    use winreg::enums::*;
    use winreg::{RegKey, RegValue};

    const HKCU_ENV: &str = "Environment";
    const HKLM_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

    /// 按操作需要的权限打开作用域键：list 只读（非管理员也能列系统变量），
    /// set/delete 需要 KEY_WRITE（系统级写入失败时提示需要管理员）。
    fn root_key(scope: &str, write: bool) -> Result<RegKey, String> {
        let flags = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        match scope {
            "user" => Ok(RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(HKCU_ENV, flags).map_err(|e| {
                format!("打开用户环境变量注册表项失败：{e}")
            })?),
            "system" => Ok(RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(HKLM_ENV, flags).map_err(|e| {
                if write {
                    // 最常见的失败：未以管理员身份运行。
                    format!("打开系统环境变量注册表项失败（系统级修改需要以管理员身份运行应用）：{e}")
                } else {
                    format!("打开系统环境变量注册表项失败：{e}")
                }
            })?),
            _ => Err(format!("未知的作用域：{scope}（仅支持 user / system）")),
        }
    }

    fn expand_value(raw: &str) -> String {
        // 展开仅在值含 '%' 时有意义；交给系统 API 以获得与资源管理器一致的
        // 结果（含嵌套引用），失败时退回原文而不是报错。
        if !raw.contains('%') {
            return raw.to_string();
        }
        // ExpandEnvironmentStringsW 要求输入以 NUL 结尾（少了它会读过堆上
        // 的相邻数据）；返回值是含结尾 NUL 的所需字符数，缓冲不足时按返回
        // 值扩容重试一次——超长 PATH 展开后会超过初始 1024。
        let wide: Vec<u16> = raw.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buffer = vec![0u16; 1024];
        loop {
            // SAFETY: 输入/输出均为合法 UTF-16 缓冲，容量与传入长度一致。
            let len = unsafe {
                windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(wide.as_ptr(), buffer.as_mut_ptr(), buffer.len() as u32)
            } as usize;
            if len == 0 {
                return raw.to_string();
            }
            if len <= buffer.len() {
                return String::from_utf16_lossy(&buffer[..len - 1]);
            }
            buffer.resize(len, 0);
        }
    }

    pub fn list(scope: &str) -> Result<Vec<EnvVar>, String> {
        let key = root_key(scope, false)?;
        let mut items = Vec::new();
        // 环境变量是该项下的「值」而非子键；enum_values 直接给出
        // (名称, RegValue)，保留了 REG_EXPAND_SZ 与 %引用%。
        for entry in key.enum_values().filter_map(|entry| entry.ok()) {
            let (name, raw) = entry;
            let (value, is_expand) = match (&raw.vtype, &raw.bytes) {
                (RegType::REG_SZ, bytes) => (utf16_or_raw(bytes), false),
                (RegType::REG_EXPAND_SZ, bytes) => (utf16_or_raw(bytes), true),
                // 非字符串类型（REG_DWORD 等）不属于本页管理范围。
                _ => continue,
            };
            let expanded = expand_value(&value);
            items.push(EnvVar {
                name,
                expanded: (expanded != value).then_some(expanded),
                value,
                is_expand,
            });
        }
        items.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
        Ok(items)
    }

    /// 注册表 REG_SZ/REG_EXPAND_SZ 的底层存储是 UTF-16；winreg 的
    /// get_raw_value 返回原始字节，这里手动解码。
    fn utf16_or_raw(bytes: &[u8]) -> String {
        if bytes.len() % 2 == 0 {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
            let trimmed: &[u16] = units.strip_suffix(&[0]).unwrap_or(&units);
            if let Ok(text) = String::from_utf16(trimmed) {
                return text;
            }
        }
        String::from_utf8_lossy(bytes).into_owned()
    }

    pub fn set(scope: &str, name: &str, value: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("变量名不能为空".into());
        }
        if name.contains('=') || name.contains('\0') {
            return Err("变量名不能包含 '=' 或空字符".into());
        }
        let key = root_key(scope, true)?;
        // 与系统对话框一致：值里含 %…% 引用时按 REG_EXPAND_SZ 存储，否则
        // REG_SZ。统一写 REG_EXPAND_SZ 会改变现有 REG_SZ 变量的语义。
        let mut bytes: Vec<u8> = Vec::with_capacity((value.len() + 1) * 2);
        for unit in value.encode_utf16().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let reg_value = RegValue {
            vtype: if value.contains('%') { RegType::REG_EXPAND_SZ } else { RegType::REG_SZ },
            bytes,
        };
        key.set_raw_value(name, &reg_value)
            .map_err(|e| format!("写入 {name} 失败：{e}"))?;
        broadcast_change();
        Ok(())
    }

    pub fn delete(scope: &str, name: &str) -> Result<(), String> {
        if PROTECTED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!("拒绝删除系统关键变量 {name}（PATH 等删除后命令行工具会全部失联）。如需调整请改为编辑值。"));
        }
        let key = root_key(scope, true)?;
        key.delete_value(name)
            .map_err(|e| format!("删除 {name} 失败：{e}"))?;
        broadcast_change();
        Ok(())
    }

    /// 写入后广播 `WM_SETTINGCHANGE("Environment")`，让资源管理器和之后
    /// 启动的进程拿到新值。接收方（如卡死的资源管理器窗口）可能拖满超时，
    /// 因此放在独立线程并限制 2 秒，避免阻塞命令线程。
    fn broadcast_change() {
        std::thread::spawn(|| {
            // SAFETY: 常量参数调用系统 API，无指针参数。
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::SendMessageTimeoutW(
                    windows_sys::Win32::UI::WindowsAndMessaging::HWND_BROADCAST,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_SETTINGCHANGE,
                    0,
                    "Environment\0".as_ptr() as isize,
                    windows_sys::Win32::UI::WindowsAndMessaging::SMTO_ABORTIFHUNG,
                    2000,
                    std::ptr::null_mut(),
                );
            }
        });
    }
}

#[cfg(windows)]
pub use imp::{delete, list, set};

#[cfg(not(windows))]
mod imp {
    pub fn list(_scope: &str) -> Result<Vec<super::EnvVar>, String> {
        Err("环境变量管理当前仅支持 Windows".into())
    }
    pub fn set(_scope: &str, _name: &str, _value: &str) -> Result<(), String> {
        Err("环境变量管理当前仅支持 Windows".into())
    }
    pub fn delete(_scope: &str, _name: &str) -> Result<(), String> {
        Err("环境变量管理当前仅支持 Windows".into())
    }
}

#[cfg(not(windows))]
pub use imp::{delete, list, set};

#[tauri::command]
pub fn env_vars_list(scope: String) -> Result<Vec<EnvVar>, String> {
    list(&scope)
}

#[tauri::command]
pub fn env_var_set(scope: String, name: String, value: String) -> Result<(), String> {
    set(&scope, &name, &value)
}

#[tauri::command]
pub fn env_var_delete(scope: String, name: String) -> Result<(), String> {
    delete(&scope, &name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn protected_names_cover_path_regardless_of_case() {
        assert!(super::PROTECTED_NAMES.contains(&"path"));
        assert!(super::PROTECTED_NAMES.contains(&"windir"));
    }

    /// 端到端走一遍真实注册表：写入临时用户变量 → 列表能读到（含 %引用%
    /// 展开预览）→ 删除后消失。只在 Windows 本机跑，变量名带明确前缀且
    /// 用例内自清理。
    #[cfg(windows)]
    #[test]
    fn user_scope_roundtrip_set_list_delete() {
        const NAME: &str = "AGENT_MANAGER_SELFTEST_TMP";
        super::set("user", NAME, "%TEMP%\\selftest").expect("set user env var");
        let listed = super::list("user").expect("list user env vars");
        let found = listed.iter().find(|item| item.name == NAME).expect("tmp var listed");
        assert_eq!(found.value, "%TEMP%\\selftest");
        assert!(found.is_expand, "value with %ref must be stored as REG_EXPAND_SZ");
        assert!(
            found.expanded.as_deref().unwrap_or_default().ends_with("\\selftest"),
            "expanded preview should resolve %TEMP%, got {:?}",
            found.expanded
        );
        super::delete("user", NAME).expect("delete tmp env var");
        let listed = super::list("user").expect("list user env vars");
        assert!(listed.iter().all(|item| item.name != NAME), "tmp var removed");

        // 超长值展开后超过初始 1024 缓冲，验证按需扩容重试路径。
        const NAME_LONG: &str = "AGENT_MANAGER_SELFTEST_LONG";
        let long_value = format!("%TEMP%\\{}", "x".repeat(1200));
        super::set("user", NAME_LONG, &long_value).expect("set long user env var");
        let listed = super::list("user").expect("list user env vars");
        let found = listed.iter().find(|item| item.name == NAME_LONG).expect("long var listed");
        let expanded = found.expanded.as_deref().expect("long var has expanded preview");
        // %TEMP% 展开为真实目录（不含 x 连串），1200 个 x 必须完整保留。
        assert!(expanded.contains('\\') && !expanded[..expanded.find("\\x").unwrap_or(0)].contains('x'), "temp dir prefix resolved");
        assert!(expanded.ends_with(&"x".repeat(1200)), "expanded preview must keep the full tail");
        super::delete("user", NAME_LONG).expect("delete long tmp env var");
    }

    #[cfg(windows)]
    #[test]
    fn protected_delete_is_refused() {
        let error = super::delete("user", "PATH").expect_err("PATH delete must be refused");
        assert!(error.contains("拒绝删除"), "got: {error}");
    }
}
