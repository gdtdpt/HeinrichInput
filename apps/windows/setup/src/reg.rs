//! 最小注册表封装。
//!
//! 只用 `RegCreateKeyExW` / `RegSetValueExW` / `RegQueryValueExW` / `RegDeleteTreeW`
//! 这四个调用，不引入注册表库。理由：安装失败时最有用的信息是**精确的
//! Win32 错误码**（`ERROR_ACCESS_DENIED` 和 `ERROR_FILE_NOT_FOUND` 的处置完全不同），
//! 而多加一层封装往往把错误揉成一句话。

use windows::core::{Error, PCWSTR};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// 转成以 NUL 结尾的 UTF-16。
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn err_text(op: &str, path: &str, code: WIN32_ERROR) -> String {
    let msg = Error::from(code).message();
    format!("{op} 失败：{path}\n    Win32 错误 {:#06x}：{msg}", code.0)
}

/// 写一个 `REG_SZ` 值。`name` 为 `None` 时写默认值。
pub fn set_string(root: HKEY, path: &str, name: Option<&str>, value: &str) -> Result<(), String> {
    let sub = wide(path);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(err_text("RegCreateKeyExW", path, rc));
    }

    let name_buf: Vec<u16> = name.map(wide).unwrap_or_else(|| vec![0]);
    let data: Vec<u16> = wide(value);
    // REG_SZ 的长度按**字节**算，包含结尾 NUL。
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    let rc = unsafe { RegSetValueExW(hkey, PCWSTR(name_buf.as_ptr()), None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if rc != ERROR_SUCCESS {
        return Err(err_text("RegSetValueExW", path, rc));
    }
    Ok(())
}

/// 读一个 `REG_SZ` 值。键或值不存在返回 `Ok(None)`，其它错误返回 `Err`。
pub fn get_string(root: HKEY, path: &str, name: Option<&str>) -> Result<Option<String>, String> {
    let sub = wide(path);
    let mut hkey = HKEY::default();
    let rc = unsafe { RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_READ, &mut hkey) };
    if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    if rc != ERROR_SUCCESS {
        return Err(err_text("RegOpenKeyExW", path, rc));
    }

    let name_buf: Vec<u16> = name.map(wide).unwrap_or_else(|| vec![0]);
    let mut ty = REG_SZ;
    let mut len: u32 = 0;
    // 先问长度（一次空读），再整块读。REG_SZ 长度是别人写的，不能假设。
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_buf.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut len),
        )
    };
    if rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Ok(None);
    }
    if rc != ERROR_SUCCESS {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Err(err_text("RegQueryValueExW(长度)", path, rc));
    }

    let mut buf = vec![0u8; len as usize + 2];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_buf.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if rc != ERROR_SUCCESS {
        return Err(err_text("RegQueryValueExW(数据)", path, rc));
    }
    if ty != REG_SZ {
        return Ok(None);
    }
    let units: Vec<u16> = buf[..len as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    Ok(Some(String::from_utf16_lossy(&units[..end])))
}

/// 键是否存在。
pub fn key_exists(root: HKEY, path: &str) -> bool {
    let sub = wide(path);
    let mut hkey = HKEY::default();
    let rc = unsafe { RegOpenKeyExW(root, PCWSTR(sub.as_ptr()), None, KEY_READ, &mut hkey) };
    if rc != ERROR_SUCCESS {
        return false;
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    true
}

/// 递归删除整棵子树。键不存在视为成功（幂等）。
pub fn delete_tree(root: HKEY, path: &str) -> Result<(), String> {
    let sub = wide(path);
    let rc = unsafe { RegDeleteTreeW(root, PCWSTR(sub.as_ptr())) };
    if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND || rc == ERROR_PATH_NOT_FOUND {
        Ok(())
    } else {
        Err(err_text("RegDeleteTreeW", path, rc))
    }
}
