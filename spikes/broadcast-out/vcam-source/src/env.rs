//! Read-only questions about this machine that decide whether a Media Foundation virtual camera can work on it. Nothing
//! here writes anywhere: the registry is opened for reading and the service manager for status queries only.

#![allow(unsafe_code)]

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegGetValueW,
    RegOpenKeyExW,
};
use windows::Win32::System::Services::{
    OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_STATUS,
};
use windows::core::HSTRING;

use crate::format::CLSID_TEXT;

/// What the checks found, as lines of text for the report.
#[derive(Debug)]
pub struct EnvReport {
    /// One finding per line.
    pub lines: Vec<String>,
}

fn read_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buffer = [0u16; 256];
    let mut size = u32::try_from(size_of_val(&buffer)).ok()?;
    // SAFETY: the buffer and size describe a valid writable region; the key and value names are valid wide strings.
    let status = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&raw mut size),
        )
    };
    (status == ERROR_SUCCESS).then(|| {
        let chars = (size as usize / 2).saturating_sub(1);
        String::from_utf16_lossy(&buffer[..chars.min(buffer.len())])
    })
}

fn read_dword(root: HKEY, key: &str, value: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: `data` is a valid 4-byte destination.
    let status = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&raw mut size),
        )
    };
    (status == ERROR_SUCCESS).then_some(data)
}

/// Whether a registry key exists (opened read-only and closed straight away).
fn key_exists(root: HKEY, key: &str) -> bool {
    let mut handle = HKEY::default();
    // SAFETY: opens read-only into a live local and closes the handle if it opened.
    unsafe {
        let status = RegOpenKeyExW(root, &HSTRING::from(key), None, KEY_READ, &raw mut handle);
        if status == ERROR_SUCCESS {
            let _ = RegCloseKey(handle);
            return true;
        }
    }
    false
}

fn service_state(name: &str) -> String {
    // SAFETY: opens the service manager and one service for status queries only, and closes both through RAII below.
    unsafe {
        let Ok(manager) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else { return "cannot query".to_owned() };
        let result = match OpenServiceW(manager, &HSTRING::from(name), SERVICE_QUERY_STATUS) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let text = if QueryServiceStatus(service, &raw mut status).is_ok() {
                    match status.dwCurrentState.0 {
                        1 => "stopped (it starts on demand when a camera is activated)".to_owned(),
                        4 => "running".to_owned(),
                        other => format!("state {other}"),
                    }
                } else {
                    "status query failed".to_owned()
                };
                let _ = windows::Win32::System::Services::CloseServiceHandle(service);
                text
            }
            Err(_) => "not installed".to_owned(),
        };
        let _ = windows::Win32::System::Services::CloseServiceHandle(manager);
        result
    }
}

/// Collects the facts.
#[must_use]
pub fn report() -> EnvReport {
    let nt = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = read_string(HKEY_LOCAL_MACHINE, nt, "CurrentBuildNumber").unwrap_or_else(|| "unknown".to_owned());
    let revision = read_dword(HKEY_LOCAL_MACHINE, nt, "UBR").map_or_else(String::new, |r| format!(".{r}"));
    let product = read_string(HKEY_LOCAL_MACHINE, nt, "ProductName").unwrap_or_default();
    let supported = build.parse::<u32>().is_ok_and(|b| b >= 22000);

    let sac = match read_dword(
        HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\CI\Policy",
        "VerifiedAndReputablePolicyState",
    ) {
        Some(0) => "off".to_owned(),
        Some(1) => "ENFORCED (unsigned or low-reputation DLLs may be blocked from loading)".to_owned(),
        Some(2) => "evaluation mode".to_owned(),
        Some(other) => format!("state {other}"),
        None => "not present".to_owned(),
    };
    let consent = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";
    let user_camera = read_string(HKEY_CURRENT_USER, consent, "Value").unwrap_or_else(|| "not set".to_owned());
    let machine_camera = read_string(HKEY_LOCAL_MACHINE, consent, "Value").unwrap_or_else(|| "not set".to_owned());

    let clsid_key = format!(r"SOFTWARE\Classes\CLSID\{CLSID_TEXT}");
    let user_clsid_key = format!(r"Software\Classes\CLSID\{CLSID_TEXT}");
    let registered_machine = key_exists(HKEY_LOCAL_MACHINE, &clsid_key);
    let registered_user = key_exists(HKEY_CURRENT_USER, &user_clsid_key);

    EnvReport {
        lines: vec![
            format!(
                "Windows: registry ProductName {product:?} (it still says Windows 10 on Windows 11), build {build}{revision}; MFCreateVirtualCamera needs build 22000 or later: {}",
                if supported { "yes" } else { "NO" }
            ),
            format!("Frame Server service (FrameServer): {}", service_state("FrameServer")),
            format!("Frame Server Monitor service (FrameServerMonitor): {}", service_state("FrameServerMonitor")),
            format!("Smart App Control: {sac}"),
            format!("Camera privacy (user): {user_camera}; (machine): {machine_camera}"),
            format!(
                "Spike CLSID {CLSID_TEXT} registered under HKLM: {registered_machine}; under HKCU: {registered_user}"
            ),
        ],
    }
}
