//! Avoid triggering Edge's “restart the existing instance without elevation?” dialog.
//! A positive acknowledgement from Windows URI activation can still lead to this prompt.
use std::ffi::OsStr;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::UI::Shell::{AssocQueryStringW, ASSOCF_IS_PROTOCOL, ASSOCSTR_PROGID};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

fn default_web_handler(scheme: &str) -> Option<String> {
    let scheme = match scheme {
        "http" => w!("http"),
        "https" => w!("https"),
        _ => return None,
    };
    let mut name = [0u16; 256];
    let mut size = name.len() as u32;
    // Use the effective user choice (including UserChoiceLatest) without changing it.
    let result = unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_PROGID,
            scheme,
            PCWSTR::null(),
            Some(PWSTR(name.as_mut_ptr())),
            &mut size,
        )
    };
    if result.0 != 0 {
        return None;
    }
    let end = name.iter().position(|&c| c == 0)?;
    (end > 0).then(|| String::from_utf16_lossy(&name[..end]))
}

fn is_elevated(pid: Pid) -> Option<bool> {
    // Only query the access token. The browser, its windows and its credentials
    // stay entirely under the user's control.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_u32());
        if process.is_null() {
            return None;
        }
        let process = OwnedHandle::from_raw_handle(process);
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0;
        let ok = GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        (ok != 0).then_some(elevation.TokenIsElevated != 0)
    }
}

fn safe_to_activate(
    handler: Option<&str>,
    edge_instances: impl FnOnce() -> Option<Vec<Option<bool>>>,
) -> bool {
    if handler.is_some_and(|name| !name.to_ascii_lowercase().starts_with("msedge")) {
        return true;
    }
    // Unknown elevation for an existing Edge browser is also a manual handoff.
    edge_instances().is_some_and(|instances| instances.iter().all(|level| *level == Some(false)))
}

pub(super) fn can_activate(scheme: &str) -> bool {
    safe_to_activate(default_web_handler(scheme).as_deref(), || {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        let session = system
            .process(Pid::from_u32(std::process::id()))?
            .session_id()?;
        let edge = OsStr::new("msedge.exe");
        let mut instances = Vec::new();
        for process in system
            .processes()
            .values()
            .filter(|p| p.name().eq_ignore_ascii_case(edge))
        {
            if process.session_id().is_some_and(|other| other != session) {
                continue;
            }
            // Chromium renderers and utility children do not own the browser session.
            if process
                .parent()
                .and_then(|pid| system.process(pid))
                .is_some_and(|parent| parent.name().eq_ignore_ascii_case(edge))
            {
                continue;
            }
            instances.push(is_elevated(process.pid()));
        }
        Some(instances)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_or_unreadable_edge_uses_manual_handoff() {
        for handler in [Some("MSEdgeHTM"), Some("MSEdgeBHTML"), None] {
            assert!(!safe_to_activate(handler, || Some(vec![
                Some(false),
                Some(true)
            ])));
            assert!(!safe_to_activate(handler, || Some(vec![None])));
            assert!(!safe_to_activate(handler, || None));
            assert!(safe_to_activate(handler, || Some(vec![Some(false)])));
            assert!(safe_to_activate(handler, || Some(vec![])));
        }
        assert!(safe_to_activate(Some("ChromeHTML"), || panic!(
            "unrelated browser"
        )));
    }
}
