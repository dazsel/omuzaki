//! The kill switch's check (design §4): whether a program on the person's protected list is running. It reads process
//! names only, never what a program is doing, and runs on the control thread every two seconds while the list is not
//! empty.

/// Lists the names of the running programs.
pub(crate) trait Processes: Send + Sync {
    /// Replaces `out` with the running programs' names.
    fn names(&self, out: &mut Vec<String>);
}

/// The first program of `protected` that runs, compared without case and without a `.exe` ending.
pub(crate) fn running(protected: &[String], processes: &dyn Processes) -> Option<String> {
    if protected.is_empty() {
        return None;
    }
    let mut names = Vec::new();
    processes.names(&mut names);
    let plain = |name: &str| {
        let name = name.trim().to_ascii_lowercase();
        name.strip_suffix(".exe").map(str::to_owned).unwrap_or(name)
    };
    let running: Vec<String> = names.iter().map(|n| plain(n)).collect();
    protected.iter().find(|p| running.contains(&plain(p))).cloned()
}

/// The OS's process list.
#[derive(Debug, Default)]
pub(crate) struct OsProcesses;

#[cfg(target_os = "linux")]
impl Processes for OsProcesses {
    #[allow(clippy::disallowed_methods, reason = "the OS's own process list in /proc, never a path an op names; the fence is for ops")]
    fn names(&self, out: &mut Vec<String>) {
        out.clear();
        let Ok(entries) = std::fs::read_dir("/proc") else { return };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit())
                && let Ok(name) = std::fs::read_to_string(entry.path().join("comm"))
            {
                out.push(name.trim_end().to_owned());
            }
        }
    }
}

#[cfg(windows)]
impl Processes for OsProcesses {
    #[allow(unsafe_code)] // the Toolhelp snapshot API: plain C calls on a handle this function owns and closes
    fn names(&self, out: &mut Vec<String>) {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
        };
        out.clear();
        // SAFETY: a process snapshot takes no pointers; the handle is checked and closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return;
        }
        // SAFETY: an all-zero PROCESSENTRY32W is a valid value; dwSize is set before use as the API asks.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0);
        // SAFETY: `entry` is a valid, sized PROCESSENTRY32W and `snapshot` a live snapshot handle.
        let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) } != 0;
        while more {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            out.push(String::from_utf16_lossy(entry.szExeFile.get(..len).unwrap_or_default()));
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot, &raw mut entry) } != 0;
        }
        // SAFETY: `snapshot` is a handle this function opened and has not closed.
        unsafe { CloseHandle(snapshot) };
    }
}

/// Elsewhere (macOS until its backend lands) the list is empty, so the kill switch never fires; the protected list
/// still saves.
#[cfg(not(any(target_os = "linux", windows)))]
impl Processes for OsProcesses {
    fn names(&self, out: &mut Vec<String>) {
        out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(&'static [&'static str]);

    impl Processes for Fixed {
        fn names(&self, out: &mut Vec<String>) {
            out.clear();
            out.extend(self.0.iter().map(|s| (*s).to_owned()));
        }
    }

    #[test]
    fn a_protected_program_is_found_without_case_or_exe() {
        let list = vec!["Vault.exe".to_owned(), "bank".to_owned()];
        assert_eq!(running(&list, &Fixed(&["shell", "vault"])), Some("Vault.exe".to_owned()));
        assert_eq!(running(&list, &Fixed(&["BANK.EXE"])), Some("bank".to_owned()));
        assert_eq!(running(&list, &Fixed(&["shell"])), None);
        assert_eq!(running(&[], &Fixed(&["vault"])), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn the_os_list_holds_this_test() {
        let mut names = Vec::new();
        OsProcesses.names(&mut names);
        assert!(names.iter().any(|n| !n.is_empty()), "no process has a name");
    }
}
