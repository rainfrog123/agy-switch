use std::process::Command;
use std::thread;
use std::time::Duration;
use sysinfo::System;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Get normalized path of the current running executable
fn get_current_exe_path() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
}

/// Helper to extract executable paths of Antigravity IDE instances
/// Uses config path as primary, and falls back to cmd() arg scanning (works on macOS/Linux)
fn get_ide_exe_paths(system: &System) -> std::collections::HashSet<String> {
    let mut immune_exe_paths = std::collections::HashSet::new();

    // Primary: load from explicit config setting (most reliable on Windows)
    if let Ok(config) = crate::modules::config::load_app_config() {
        if let Some(ide_path) = config.antigravity_ide_executable {
            if let Ok(canonical) = std::path::PathBuf::from(&ide_path).canonicalize() {
                immune_exe_paths.insert(canonical.to_string_lossy().to_lowercase());
            } else {
                immune_exe_paths.insert(ide_path.to_lowercase());
            }
        }
    }

    // Fallback: scan process cmd() args (works on macOS/Linux, may be empty on Windows)
    for (_pid, process) in system.processes() {
        let args = process.cmd();
        let args_str = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_lowercase())
            .collect::<Vec<String>>()
            .join(" ");

        if args_str.contains("antigravity ide") || args_str.contains("antigravity-ide") {
            if let Some(exe_path) = process.exe().and_then(|p| p.to_str()) {
                immune_exe_paths.insert(exe_path.to_lowercase());
            }
        }
    }
    immune_exe_paths
}

/// Check if a process with the given name is running (case-insensitive, strips .exe on Windows).
pub fn is_process_running_by_name(target_name: &str) -> bool {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let target_lower = target_name.to_lowercase();
    for (_pid, process) in system.processes() {
        let mut name = process.name().to_string_lossy().to_lowercase();
        if name.ends_with(".exe") {
            name.truncate(name.len() - 4);
        }
        if name == target_lower {
            return true;
        }
    }
    false
}

/// Account switchers live in folders whose path contains "antigravity", but they are not the app.
fn is_manager_process(name: &str, exe_path: &str) -> bool {
    let name = name.to_lowercase();
    let exe = exe_path.to_lowercase().replace('/', "\\");
    name.starts_with("agy-switch")
        || name.starts_with("antigravity-tools")
        || name.starts_with("antigravity_tools")
        || exe.contains("agy-switch")
        || exe.contains("antigravity-tools")
        || exe.contains("antigravity_tools")
        || exe.contains("antigravity-manager")
}

/// The desktop app binary. A switcher exe in a similarly named folder is not a launch target.
fn is_antigravity_app_exe(path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let exe = path.to_string_lossy();
    (name.eq_ignore_ascii_case("Antigravity.exe") || name.eq_ignore_ascii_case("Antigravity"))
        && !is_manager_process(name, &exe)
}

/// Helper process discriminator to filter out sub-processes, audio/gpu/renderers, crashpads, and language servers
pub(crate) fn is_helper_process(name: &str, args_str: &str, exe_path: &str) -> bool {
    let name_lower = name.to_lowercase();
    let args_lower = args_str.to_lowercase();
    let exe_lower = exe_path.to_lowercase();

    args_lower.contains("--type=")
        || args_lower.contains("node-ipc")
        || args_lower.contains("nodeipc")
        || args_lower.contains("max-old-space-size")
        || args_lower.contains("node_modules")
        || args_lower.contains("--standalone")
        || args_lower.contains("--subclient_type")
        || args_lower.contains("--override_ide_name")
        || name_lower.contains("helper")
        || name_lower.contains("plugin")
        || name_lower.contains("renderer")
        || name_lower.contains("gpu")
        || name_lower.contains("crashpad")
        || name_lower.contains("utility")
        || name_lower.contains("audio")
        || name_lower.contains("sandbox")
        || name_lower.contains("language_server")
        || args_lower.contains("language_server")
        || exe_lower.contains("crashpad")
        || exe_lower.contains("helper")
        || exe_lower.contains("language_server")
}

/// Sanitize restart arguments to prevent internal engine/language_server arguments
/// (such as --standalone or --override_ide_name) from leaking into relaunch commands.
pub(crate) fn sanitize_restart_args(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| {
            let lower = arg.trim().to_lowercase();
            !lower.is_empty()
                && !lower.starts_with("--standalone")
                && !lower.starts_with("--override_ide_name")
                && !lower.starts_with("--subclient_type")
                && !lower.starts_with("--remote-debugging-port=")
                && !lower.contains("language_server")
        })
        .cloned()
        .collect()
}

/// Extra cleanup: Kill orphan language_server processes located inside the Antigravity installation
pub fn sweep_orphan_language_servers() {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);

    for (pid, process) in system.processes() {
        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path = process
            .exe()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_lowercase();

        if (name.contains("language_server") || exe_path.contains("language_server"))
            && exe_path.contains("antigravity")
            && !exe_path.contains("antigravity ide")
            && !exe_path.contains("antigravity-ide")
        {
            let pid_u32 = pid.as_u32();
            crate::modules::logger::log_info(&format!(
                "Sweeping orphan language_server process (PID: {}, Path: {})",
                pid_u32, exe_path
            ));
            #[cfg(target_os = "windows")]
            {
                let _ = Command::new("taskkill")
                    .args(["/F", "/PID", &pid_u32.to_string()])
                    .creation_flags(0x08000000)
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = Command::new("kill")
                    .args(["-9", &pid_u32.to_string()])
                    .output();
            }
        }
    }
}


/// Check if Antigravity is running
pub fn is_antigravity_running(target_ide: Option<&str>) -> bool {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let ide_exe_paths = get_ide_exe_paths(&system);

    let current_exe = get_current_exe_path();
    let current_pid = std::process::id();

    // Load both manual paths from config
    let config = crate::modules::config::load_app_config().ok();
    let manual_path = config
        .as_ref()
        .and_then(|c| c.antigravity_executable.as_ref())
        .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());
    let ide_manual_path = config
        .as_ref()
        .and_then(|c| c.antigravity_ide_executable.as_ref())
        .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());

    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 == current_pid {
            continue;
        }

        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path = process
            .exe()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_lowercase();

        if is_manager_process(&name, &exe_path) {
            continue;
        }

        // Exclude own path (handles case where manager is mistaken for Antigravity on Linux)
        if let (Some(ref my_path), Some(p_exe)) = (&current_exe, process.exe()) {
            if let Ok(p_path) = p_exe.canonicalize() {
                if my_path == &p_path {
                    continue;
                }
            }
        }

        // Common helper process exclusion logic
        let args = process.cmd();
        let args_str = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_lowercase())
            .collect::<Vec<String>>()
            .join(" ");

        if is_helper_process(&name, &args_str, &exe_path) {
            continue;
        }

        // Recognition ref 2: If targeting IDE and ide_manual_path is configured, check it first
        if target_ide == Some("ide") {
            if let (Some(ref ide_m_path), Some(p_exe)) = (&ide_manual_path, process.exe()) {
                if let Ok(p_path) = p_exe.canonicalize() {
                    #[cfg(target_os = "macos")]
                    {
                        let m = ide_m_path.to_string_lossy();
                        let p = p_path.to_string_lossy();
                        if let (Some(mi), Some(pi)) = (m.find(".app"), p.find(".app")) {
                            if m[..mi + 4] == p[..pi + 4] {
                                return true;
                            }
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    if ide_m_path == &p_path {
                        return true;
                    }
                }
            }
        }

        // Recognition ref 3: Priority check for manual path match (client)
        if target_ide != Some("ide") {
            if let (Some(ref m_path), Some(p_exe)) = (&manual_path, process.exe()) {
                if let Ok(p_path) = p_exe.canonicalize() {
                    // macOS: Check if within the same .app bundle
                    #[cfg(target_os = "macos")]
                    {
                        let m_path_str = m_path.to_string_lossy();
                        let p_path_str = p_path.to_string_lossy();
                        if let (Some(m_idx), Some(p_idx)) =
                            (m_path_str.find(".app"), p_path_str.find(".app"))
                        {
                            if m_path_str[..m_idx + 4] == p_path_str[..p_idx + 4] {
                                return true;
                            }
                        }
                    }

                    #[cfg(not(target_os = "macos"))]
                    if m_path == &p_path {
                        return true;
                    }
                }
            }
        }

        // 3. Strict mode: If the relevant manual path is configured, we strictly enforce it
        // and DO NOT fallback to fuzzy string matching.
        if manual_path.is_some() && target_ide != Some("ide") {
            continue;
        }
        if ide_manual_path.is_some() && target_ide == Some("ide") {
            continue;
        }

        // Check if the process matches target_ide
        let is_ide_match = if target_ide == Some("ide") {
            exe_path.contains("antigravity ide")
                || exe_path.contains("antigravity-ide")
                || name.contains("antigravity ide")
                || name.contains("antigravity-ide")
                || ide_exe_paths.contains(&exe_path)
        } else {
            if ide_exe_paths.contains(&exe_path) {
                false // Explicitly immune (it is an IDE)
            } else {
                (exe_path.contains("antigravity") || name.contains("antigravity"))
                    && !exe_path.contains("antigravity ide")
                    && !exe_path.contains("antigravity-ide")
                    && !name.contains("antigravity ide")
                    && !name.contains("antigravity-ide")
            }
        };

        if is_ide_match {
            return true;
        }
    }

    false
}

#[cfg(target_os = "linux")]
/// Get PID set of current process and all ancestors.
/// Only ancestors (parents/grandparents) are excluded to prevent accidentally killing
/// the launcher or shell that started the Manager.
/// Child processes spawned by the Manager (e.g., the IDE) must remain killable, so
/// descendants are intentionally NOT included here.
fn get_self_family_pids(system: &sysinfo::System) -> std::collections::HashSet<u32> {
    let current_pid = std::process::id();
    let mut family_pids = std::collections::HashSet::new();
    family_pids.insert(current_pid);

    // Traverse upward to find all ancestors - prevent killing the launcher/shell
    let mut next_pid = current_pid;
    // Prevent infinite loop, max depth 10
    for _ in 0..10 {
        let pid_val = sysinfo::Pid::from_u32(next_pid);
        if let Some(process) = system.process(pid_val) {
            if let Some(parent) = process.parent() {
                let parent_id = parent.as_u32();
                // Avoid cycles or duplicates
                if !family_pids.insert(parent_id) {
                    break;
                }
                next_pid = parent_id;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    family_pids
}

/// Get PIDs of all Antigravity processes (including main and helper processes)
#[cfg(not(target_os = "windows"))]
fn get_antigravity_pids(target_ide: Option<&str>) -> Vec<u32> {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let ide_exe_paths = get_ide_exe_paths(&system);

    // Linux: Enable family process tree exclusion
    #[cfg(target_os = "linux")]
    let family_pids = get_self_family_pids(&system);

    let mut pids = Vec::new();
    let current_pid = std::process::id();
    let current_exe = get_current_exe_path();

    // Load both manual paths from config
    let config = crate::modules::config::load_app_config().ok();
    let manual_path = config
        .as_ref()
        .and_then(|c| c.antigravity_executable.as_ref())
        .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());
    let ide_manual_path = config
        .as_ref()
        .and_then(|c| c.antigravity_ide_executable.as_ref())
        .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());

    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();

        // Exclude own PID
        if pid_u32 == current_pid {
            continue;
        }

        // Exclude own executable path (hardened against broad name matching)
        if let (Some(ref my_path), Some(p_exe)) = (&current_exe, process.exe()) {
            if let Ok(p_path) = p_exe.canonicalize() {
                if my_path == &p_path {
                    continue;
                }
            }
        }

        let name = process.name().to_string_lossy().to_lowercase();

        #[cfg(target_os = "linux")]
        {
            // 1. Exclude family processes (self, children, parents)
            if family_pids.contains(&pid_u32) {
                continue;
            }
            // 2. Extra protection: match "tools" likely manager if not a child
            if name.contains("tools") {
                continue;
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            // Other platforms: exclude only self
            if pid_u32 == current_pid {
                continue;
            }
        }

        // Recognition ref IDE manual path: If the process exactly matches the configured IDE path,
        // NEVER add it to kill list regardless of target_ide
        if let (Some(ref ide_m_path), Some(p_exe)) = (&ide_manual_path, process.exe()) {
            if let Ok(p_path) = p_exe.canonicalize() {
                #[cfg(target_os = "macos")]
                let matches = {
                    let m = ide_m_path.to_string_lossy();
                    let p = p_path.to_string_lossy();
                    matches!(m.find(".app").zip(p.find(".app")), Some((mi, pi)) if m[..mi + 4] == p[..pi + 4])
                };
                #[cfg(not(target_os = "macos"))]
                let matches = ide_m_path == &p_path;

                if matches && target_ide != Some("ide") {
                    // This is explicitly the IDE we must NOT kill when switching client
                    continue;
                }
                if matches && target_ide == Some("ide") {
                    // This is the IDE we WANT to close
                    pids.push(pid_u32);
                    continue;
                }
            }
        }

        // Recognition ref 3: Check manual config path match (client)
        if let (Some(ref m_path), Some(p_exe)) = (&manual_path, process.exe()) {
            if let Ok(p_path) = p_exe.canonicalize() {
                #[cfg(target_os = "macos")]
                let matches = {
                    let m_path_str = m_path.to_string_lossy();
                    let p_path_str = p_path.to_string_lossy();
                    matches!(m_path_str.find(".app").zip(p_path_str.find(".app")), Some((m_idx, p_idx)) if m_path_str[..m_idx + 4] == p_path_str[..p_idx + 4])
                };
                #[cfg(not(target_os = "macos"))]
                let matches = m_path == &p_path;

                if matches {
                    #[cfg(target_os = "macos")]
                    let is_main = {
                        let args = process.cmd();
                        let is_helper_by_args = args
                            .iter()
                            .any(|arg| arg.to_string_lossy().contains("--type="));
                        let is_helper_by_name = name.contains("helper")
                            || name.contains("plugin")
                            || name.contains("renderer")
                            || name.contains("gpu")
                            || name.contains("crashpad")
                            || name.contains("utility")
                            || name.contains("audio")
                            || name.contains("sandbox");
                        !is_helper_by_args && !is_helper_by_name
                    };
                    #[cfg(not(target_os = "macos"))]
                    let is_main = true;

                    if is_main {
                        if target_ide == Some("ide") {
                            // This is explicitly the client we must NOT kill when switching IDE
                            continue;
                        } else {
                            // This is the client we WANT to close
                            pids.push(pid_u32);
                            continue;
                        }
                    }
                }
            }
        }

        // 4. Strict mode: If the relevant manual path is configured, we strictly enforce it
        // and DO NOT fallback to fuzzy string matching.
        if manual_path.is_some() && target_ide != Some("ide") {
            continue;
        }
        if ide_manual_path.is_some() && target_ide == Some("ide") {
            continue;
        }

        // Get executable path
        let exe_path = process
            .exe()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Common helper process exclusion logic
        let args = process.cmd();
        let args_str = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_lowercase())
            .collect::<Vec<String>>()
            .join(" ");

        let is_helper = args_str.contains("--type=")
            || name.contains("helper")
            || name.contains("plugin")
            || name.contains("renderer")
            || name.contains("gpu")
            || name.contains("crashpad")
            || name.contains("utility")
            || name.contains("audio")
            || name.contains("sandbox")
            || exe_path.contains("crashpad");

        // Check if the process matches target_ide
        let is_ide_match = if target_ide == Some("ide") {
            exe_path.contains("antigravity ide")
                || exe_path.contains("antigravity-ide")
                || name.contains("antigravity ide")
                || name.contains("antigravity-ide")
                || ide_exe_paths.contains(&exe_path)
        } else {
            if ide_exe_paths.contains(&exe_path) {
                false // Explicitly immune (it is an IDE)
            } else {
                (exe_path.contains("antigravity") || name.contains("antigravity"))
                    && !exe_path.contains("antigravity ide")
                    && !exe_path.contains("antigravity-ide")
                    && !name.contains("antigravity ide")
                    && !name.contains("antigravity-ide")
            }
        };

        if is_ide_match && !is_helper {
            pids.push(pid_u32);
        }
    }

    if !pids.is_empty() {
        crate::modules::logger::log_info(&format!(
            "Found {} Antigravity ({:?}) processes: {:?}",
            pids.len(),
            target_ide,
            pids
        ));
    }

    pids
}

/// Processes that belong to the installed Antigravity app, including helpers and language_server.
/// Switcher binaries are excluded even when their path contains "antigravity".
#[cfg(target_os = "windows")]
fn windows_product_pids(target_ide: Option<&str>) -> Vec<u32> {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let current = std::process::id();
    let mut pids = Vec::new();
    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 == current {
            continue;
        }
        let name = process.name().to_string_lossy().to_lowercase();
        if name.starts_with("agy-switch") || name.starts_with("antigravity-tools") {
            continue;
        }
        let exe = process
            .exe()
            .map(|path| path.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .replace('/', "\\");
        if exe.contains("agy-switch")
            || exe.contains("antigravity-tools")
            || exe.contains("antigravity-manager")
        {
            continue;
        }
        let is_ide = exe.contains("\\antigravity ide\\")
            || exe.contains("\\antigravity-ide\\")
            || name.contains("antigravity ide")
            || name.contains("antigravity-ide");
        let is_app = exe.contains("\\antigravity\\")
            || name == "antigravity.exe"
            || name == "antigravity";
        let matched = if target_ide == Some("ide") {
            is_ide
        } else {
            is_app && !is_ide
        };
        if matched {
            pids.push(pid_u32);
        }
    }
    pids
}

#[cfg(target_os = "windows")]
fn terminate_windows_pid(pid: u32) -> Result<(), String> {
    use std::ffi::c_void;
    #[link(name = "Kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn TerminateProcess(handle: *mut c_void, code: u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const PROCESS_TERMINATE: u32 = 0x0001;
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            let err = GetLastError();
            // 87: already exited. 5: access denied.
            if err == 87 {
                return Ok(());
            }
            return Err(format!("OpenProcess failed ({err})"));
        }
        let ok = TerminateProcess(handle, 1);
        let err = GetLastError();
        CloseHandle(handle);
        if ok == 0 && err != 87 {
            return Err(format!("TerminateProcess failed ({err})"));
        }
    }
    Ok(())
}

/// Close Antigravity processes
pub fn close_antigravity(timeout_secs: u64, target_ide: Option<&str>) -> Result<(), String> {
    crate::modules::logger::log_info(&format!("Closing Antigravity ({:?})...", target_ide));

    #[cfg(target_os = "windows")]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
        let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let taskkill = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("taskkill.exe");
        loop {
            let pids = windows_product_pids(target_ide);
            if pids.is_empty() {
                break;
            }
            crate::modules::logger::log_info(&format!(
                "Precisely closing {} Antigravity processes on Windows: {:?}",
                pids.len(),
                pids
            ));
            for pid in &pids {
                if let Err(err) = terminate_windows_pid(*pid) {
                    crate::modules::logger::log_warn(&format!(
                        "TerminateProcess PID {pid} failed: {err}"
                    ));
                }
                let output = Command::new(&taskkill)
                    .args(["/F", "/T", "/PID", &pid.to_string()])
                    .creation_flags(0x08000000)
                    .output();
                if let Ok(output) = output {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stderr = stderr.trim();
                        if !stderr.is_empty() && !stderr.to_lowercase().contains("not found") {
                            crate::modules::logger::log_warn(&format!(
                                "taskkill PID {pid}: {stderr}"
                            ));
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(400));
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        let remaining = windows_product_pids(target_ide);
        if !remaining.is_empty() {
            return Err(format!(
                "Unable to close Antigravity (still running: {:?}). Close it and retry.",
                remaining
            ));
        }
    }

    #[cfg(target_os = "macos")]
    {
        // macOS: Optimize closing strategy to avoid "Window terminated unexpectedly" popups
        // Strategy: SEND SIGTERM to main process only, let it coordinate closing children

        let pids = get_antigravity_pids(target_ide);
        if !pids.is_empty() {
            // 1. Identify main process (PID)
            let mut system = System::new();
            system.refresh_processes(sysinfo::ProcessesToUpdate::All);

            let mut main_pid = None;

            // Load manual configuration path as highest priority reference
            let manual_path = crate::modules::config::load_app_config()
                .ok()
                .and_then(|c| c.antigravity_executable)
                .and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());

            crate::modules::logger::log_info("Analyzing process list to identify main process:");
            for pid_u32 in &pids {
                let pid = sysinfo::Pid::from_u32(*pid_u32);
                if let Some(process) = system.process(pid) {
                    let name = process.name().to_string_lossy();
                    let args = process.cmd();
                    let args_str = args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<String>>()
                        .join(" ");

                    crate::modules::logger::log_info(&format!(
                        " - PID: {} | Name: {}", pid_u32, name
                    ));

                    // 1. Priority to manual path matching
                    if let (Some(ref m_path), Some(p_exe)) = (&manual_path, process.exe()) {
                        if let Ok(p_path) = p_exe.canonicalize() {
                            let m_path_str = m_path.to_string_lossy();
                            let p_path_str = p_path.to_string_lossy();
                            if let (Some(m_idx), Some(p_idx)) =
                                (m_path_str.find(".app"), p_path_str.find(".app"))
                            {
                                if m_path_str[..m_idx + 4] == p_path_str[..p_idx + 4] {
                                    // Deep validation: even if path matches, must exclude Helper keywords and arguments
                                    let is_helper_by_args = args_str.contains("--type=");
                                    let is_helper_by_name = name.to_lowercase().contains("helper")
                                        || name.to_lowercase().contains("plugin")
                                        || name.to_lowercase().contains("renderer")
                                        || name.to_lowercase().contains("gpu")
                                        || name.to_lowercase().contains("crashpad")
                                        || name.to_lowercase().contains("utility")
                                        || name.to_lowercase().contains("audio")
                                        || name.to_lowercase().contains("sandbox")
                                        || name.to_lowercase().contains("language_server");

                                    if !is_helper_by_args && !is_helper_by_name {
                                        main_pid = Some(pid_u32);
                                        crate::modules::logger::log_info(&format!(
                                            "   => Identified as main process (manual path match)"
                                        ));
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    // 2. Feature analysis matching (fallback)
                    let is_helper_by_name = name.to_lowercase().contains("helper")
                        || name.to_lowercase().contains("crashpad")
                        || name.to_lowercase().contains("utility")
                        || name.to_lowercase().contains("audio")
                        || name.to_lowercase().contains("sandbox")
                        || name.to_lowercase().contains("language_server")
                        || name.to_lowercase().contains("plugin")
                        || name.to_lowercase().contains("renderer");

                    let is_helper_by_args = args_str.contains("--type=");

                    if !is_helper_by_name && !is_helper_by_args {
                        if main_pid.is_none() {
                            main_pid = Some(pid_u32);
                            crate::modules::logger::log_info(&format!(
                                "   => Identified as main process (Name/Args analysis)"
                            ));
                        }
                    } else {
                        crate::modules::logger::log_info(&format!(
                            "   => Identified as helper process (Helper/Args)"
                        ));
                    }
                }
            }

            // Phase 1: Graceful exit (SIGTERM)
            if let Some(pid) = main_pid {
                crate::modules::logger::log_info(&format!(
                    "Sending SIGTERM to main process PID: {}",
                    pid
                ));
                let _ = Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .output();
            } else {
                crate::modules::logger::log_warn(
                    "No main process identified, sending SIGTERM to all associated processes",
                );
                for pid in &pids {
                    let _ = Command::new("kill")
                        .args(["-15", &pid.to_string()])
                        .output();
                }
            }

            // Wait for graceful exit (max 70% of timeout_secs)
            let graceful_timeout = (timeout_secs * 7) / 10;
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(graceful_timeout) {
                if !is_antigravity_running(target_ide) {
                    crate::modules::logger::log_info("All Antigravity processes gracefully closed");
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(500));
            }

            // Phase 2: Force kill (SIGKILL) - targeting all remaining processes (Helpers)
            if is_antigravity_running(target_ide) {
                let remaining_pids = get_antigravity_pids(target_ide);
                if !remaining_pids.is_empty() {
                    crate::modules::logger::log_warn(&format!(
                        "Graceful exit timeout, force killing {} remaining processes (SIGKILL)",
                        remaining_pids.len()
                    ));
                    for pid in &remaining_pids {
                        let output = Command::new("kill").args(["-9", &pid.to_string()]).output();

                        if let Ok(result) = output {
                            if !result.status.success() {
                                let error = String::from_utf8_lossy(&result.stderr);
                                if !error.contains("No such process") {
                                    // "No matching processes" for killall, "No such process" for kill
                                    crate::modules::logger::log_error(&format!(
                                        "SIGKILL process {} failed: {}",
                                        pid, error
                                    ));
                                }
                            }
                        }
                    }
                    thread::sleep(Duration::from_secs(1));
                }

                // Final check
                if !is_antigravity_running(target_ide) {
                    crate::modules::logger::log_info("All processes exited after forced cleanup");
                    return Ok(());
                }
            } else {
                crate::modules::logger::log_info("All processes exited after SIGTERM");
                return Ok(());
            }
        } else {
            // Only consider not running when pids is empty, don't error here as it might already be closed
            crate::modules::logger::log_info("Antigravity not running, no need to close");
            return Ok(());
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux: precise closing
        let pids = get_antigravity_pids(target_ide);
        if !pids.is_empty() {
            let mut system = System::new();
            system.refresh_processes(sysinfo::ProcessesToUpdate::All);

            let mut main_pid = None;

            for pid_u32 in &pids {
                let pid = sysinfo::Pid::from_u32(*pid_u32);
                if let Some(process) = system.process(pid) {
                    let name = process.name().to_string_lossy();
                    let args = process.cmd();
                    let args_str = args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<String>>()
                        .join(" ");

                    let is_helper_by_name = name.to_lowercase().contains("helper")
                        || name.to_lowercase().contains("crashpad")
                        || name.to_lowercase().contains("utility")
                        || name.to_lowercase().contains("audio")
                        || name.to_lowercase().contains("sandbox")
                        || name.to_lowercase().contains("plugin")
                        || name.to_lowercase().contains("renderer");

                    let is_helper_by_args = args_str.contains("--type=");

                    if !is_helper_by_name && !is_helper_by_args {
                        main_pid = Some(pid_u32);
                        break;
                    }
                }
            }

            // Phase 1: SIGTERM
            if let Some(pid) = main_pid {
                let _ = Command::new("kill")
                    .args(["-15", &pid.to_string()])
                    .output();
            } else {
                crate::modules::logger::log_warn(
                    "No clear Linux main process identified, sending SIGTERM to all associated processes",
                );
                for pid in &pids {
                    let _ = Command::new("kill")
                        .args(["-15", &pid.to_string()])
                        .output();
                }
            }

            // Wait for graceful exit
            let graceful_timeout = (timeout_secs * 7) / 10;
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(graceful_timeout) {
                if !is_antigravity_running(target_ide) {
                    crate::modules::logger::log_info("Antigravity gracefully closed");
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(500));
            }

            // Phase 2: SIGKILL
            if is_antigravity_running(target_ide) {
                let remaining_pids = get_antigravity_pids(target_ide);
                if !remaining_pids.is_empty() {
                    crate::modules::logger::log_warn(&format!(
                        "Graceful exit timeout, force killing {} remaining processes (SIGKILL)",
                        remaining_pids.len()
                    ));
                    for pid in &remaining_pids {
                        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
                    }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        } else {
            crate::modules::logger::log_info(
                "No Antigravity processes found to close (possibly filtered or not running)",
            );
        }
    }

    // Final check. Windows already verified the full process tree, including helpers.
    #[cfg(not(target_os = "windows"))]
    if is_antigravity_running(target_ide) {
        return Err(
            "Unable to close Antigravity process, please close manually and retry".to_string(),
        );
    }

    crate::modules::logger::log_info("Antigravity closed successfully");
    Ok(())
}

/// Clean AppImage-specific environment variables before spawning external processes on Linux
#[cfg(target_os = "linux")]
pub fn clean_appimage_env(cmd: &mut Command) {
    let appimage_vars = [
        "APPIMAGE",
        "APPDIR",
        "ARGV0",
        "OWD",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "GTK_PATH",
        "GIO_EXTRA_MODULES",
        "GI_TYPELIB_PATH",
        "QT_PLUGIN_PATH",
        "QT_QPA_PLATFORM_PLUGIN_PATH",
    ];
    for var in &appimage_vars {
        cmd.env_remove(var);
    }

    if let Ok(xdg_data_dirs) = std::env::var("XDG_DATA_DIRS") {
        let filtered_dirs: Vec<&str> = xdg_data_dirs
            .split(':')
            .filter(|dir| !dir.starts_with("/tmp/.mount_"))
            .collect();
        cmd.env("XDG_DATA_DIRS", filtered_dirs.join(":"));
    }
}

/// Start Antigravity using the GUI's existing stdio behavior.
pub fn start_antigravity(target_ide: Option<&str>) -> Result<(), String> {
    start_antigravity_with_stdio(target_ide, false, None)
}

/// CLI launches must not leak child output or keep the CLI's capture pipes open.
#[allow(dead_code)]
pub fn start_antigravity_detached(target_ide: Option<&str>) -> Result<(), String> {
    start_antigravity_with_stdio(target_ide, true, None)
}

#[allow(dead_code)]
pub fn start_antigravity_with_extra_args(
    target_ide: Option<&str>,
    extra_args: &[&str],
) -> Result<(), String> {
    start_antigravity_with_stdio(target_ide, false, Some(extra_args))
}

/// Fully close Antigravity processes, ensuring all file locks and background helpers are terminated.
/// Waits 2 seconds for clean teardown.
pub fn fully_close_antigravity(target_ide: Option<&str>) -> Result<(), String> {
    crate::modules::logger::log_info(&format!("Fully closing Antigravity ({:?})...", target_ide));

    close_antigravity(8, target_ide)?;
    if target_ide.is_some() {
        close_antigravity(8, None)?;
    }

    // Extra cleanup: Kill orphan language_server processes
    sweep_orphan_language_servers();

    // Wait for file handles to flush after the process tree is gone.
    thread::sleep(Duration::from_secs(2));

    #[cfg(target_os = "windows")]
    {
        let remaining = windows_product_pids(target_ide);
        if !remaining.is_empty() {
            return Err(format!(
                "Unable to close Antigravity (still running: {:?}). Close it and retry.",
                remaining
            ));
        }
    }

    Ok(())
}

/// Launch Antigravity with --remote-debugging-port=9222
#[allow(dead_code)]
pub fn launch_antigravity_9222(target_ide: Option<&str>) -> Result<(), String> {
    launch_antigravity_9222_with_fallback(target_ide, None, None)
}

/// Launch Antigravity with --remote-debugging-port=9222, prioritizing pre-snapshot path and arguments
pub fn launch_antigravity_9222_with_fallback(
    target_ide: Option<&str>,
    active_exe: Option<&std::path::Path>,
    active_args: Option<&[String]>,
) -> Result<(), String> {
    crate::modules::logger::log_info(&format!(
        "Launching Antigravity with --remote-debugging-port=9222 ({:?})...",
        target_ide
    ));

    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let default_path = std::path::PathBuf::from(&local)
            .join("Programs")
            .join("Antigravity")
            .join("Antigravity.exe");

        let snapshot = active_exe.filter(|path| path.exists() && is_antigravity_app_exe(path));
        let detected = get_antigravity_executable_path(target_ide)
            .filter(|path| is_antigravity_app_exe(path));
        let exe = if let Some(path) = snapshot {
            path.to_path_buf()
        } else if default_path.exists() {
            default_path
        } else if let Some(path) = detected {
            path
        } else {
            return Err("Antigravity executable not found on system".to_string());
        };

        let mut cmd = Command::new(&exe);
        if let Some(parent) = exe.parent() {
            cmd.current_dir(parent);
        }

        // Apply sanitized arguments
        if let Some(args) = active_args {
            let clean = sanitize_restart_args(args);
            for arg in clean {
                cmd.arg(arg);
            }
        } else if let Ok(config) = crate::modules::config::load_app_config() {
            if let Some(ref args) = config.antigravity_args {
                let clean = sanitize_restart_args(args);
                for arg in clean {
                    cmd.arg(arg);
                }
            }
        }
        cmd.arg("--remote-debugging-port=9222");

        cmd.spawn()
            .map_err(|e| format!("Failed to start Antigravity: {}", e))?;

        crate::modules::logger::log_info(&format!(
            "Antigravity started with --remote-debugging-port=9222: {:?}",
            exe
        ));
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        start_antigravity_with_extra_args(target_ide, &["--remote-debugging-port=9222"])
    }
}

/// Restart Antigravity on debugging port 9222:
/// (Stop-Process -Name "Antigravity" -Force -ErrorAction SilentlyContinue; Start-Sleep -Seconds 2; & "$env:LOCALAPPDATA\Programs\Antigravity\Antigravity.exe" --remote-debugging-port=9222)
pub fn restart_antigravity_9222() -> Result<(), String> {
    let active_exe = get_antigravity_executable_path(None);
    let active_args = get_args_from_running_process(None);
    fully_close_antigravity(None)?;
    launch_antigravity_9222_with_fallback(None, active_exe.as_deref(), active_args.as_deref())
}

fn prepare_launch_stdio(command: &mut Command, detached: bool) -> Result<(), String> {
    if detached {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        prevent_cli_pipe_inheritance()?;
    }
    Ok(())
}

#[cfg(windows)]
fn prevent_cli_pipe_inheritance() -> Result<(), String> {
    use std::ffi::c_void;
    #[link(name = "Kernel32")]
    extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn GetFileType(handle: *mut c_void) -> u32;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    // Stable Rust's Command inherits other inheritable process handles even
    // when its own stdio is NUL. A Node/PowerShell capture pipe can therefore
    // stay open in the APP. Clear only this CLI process's pipe inheritance;
    // the handles remain open/usable by the CLI and GUI launches never run this.
    // Console handles are deliberately untouched (SetHandleInformation is not
    // supported for all console handle types).
    for kind in [-10_i32, -11_i32, -12_i32] {
        unsafe {
            let handle = GetStdHandle(kind as u32);
            if !handle.is_null() && handle as isize != -1 && GetFileType(handle) == 3 {
                if SetHandleInformation(handle, 1, 0) == 0 {
                    return Err(format!(
                        "Cannot isolate CLI capture pipe: {}",
                        std::io::Error::last_os_error()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[allow(unused_mut)]
fn start_antigravity_with_stdio(
    target_ide: Option<&str>,
    detached: bool,
    extra_args: Option<&[&str]>,
) -> Result<(), String> {
    crate::modules::logger::log_info(&format!("Starting Antigravity ({:?})...", target_ide));

    // Prefer manually specified path and args from configuration
    let config = crate::modules::config::load_app_config().ok();
    let manual_path = if target_ide == Some("ide") {
        config
            .as_ref()
            .and_then(|c| c.antigravity_ide_executable.clone())
    } else {
        config
            .as_ref()
            .and_then(|c| c.antigravity_executable.clone())
    };
    let args = config.and_then(|c| c.antigravity_args.clone());

    if let Some(mut path_str) = manual_path {
        let mut path = std::path::PathBuf::from(&path_str);

        #[cfg(target_os = "macos")]
        {
            // Fault tolerance: If path is inside .app bundle (e.g. misselected Helper), auto-correct to .app directory
            if let Some(app_idx) = path_str.find(".app") {
                let corrected_app = &path_str[..app_idx + 4];
                if corrected_app != path_str {
                    crate::modules::logger::log_info(&format!(
                        "Detected macOS path inside .app bundle, auto-correcting to: {}",
                        corrected_app
                    ));
                    path_str = corrected_app.to_string();
                    path = std::path::PathBuf::from(&path_str);
                }
            }
        }

        if path.exists() {
            crate::modules::logger::log_info(&format!(
                "Starting with manual configuration path: {}",
                path_str
            ));

            #[cfg(target_os = "macos")]
            {
                // macOS: if .app directory, use open
                if path_str.ends_with(".app") || path.is_dir() {
                    let mut cmd = Command::new("open");
                    prepare_launch_stdio(&mut cmd, detached)?;
                    cmd.arg("-a").arg(&path_str);

                    // Add startup arguments
                    if let Some(ref args) = args {
                        for arg in args {
                            cmd.arg(arg);
                        }
                    }
                    if let Some(extras) = extra_args {
                        for arg in extras {
                            cmd.arg(arg);
                        }
                    }

                    cmd.spawn()
                        .map_err(|e| format!("Startup failed (open): {}", e))?;
                } else {
                    let mut cmd = Command::new(&path_str);
                    prepare_launch_stdio(&mut cmd, detached)?;

                    // Add startup arguments
                    if let Some(ref args) = args {
                        for arg in args {
                            cmd.arg(arg);
                        }
                    }
                    if let Some(extras) = extra_args {
                        for arg in extras {
                            cmd.arg(arg);
                        }
                    }

                    cmd.spawn()
                        .map_err(|e| format!("Startup failed (direct): {}", e))?;
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                let mut cmd = Command::new(&path_str);
                if let Some(parent) = path.parent() {
                    cmd.current_dir(parent);
                }
                prepare_launch_stdio(&mut cmd, detached)?;

                // Add startup arguments
                if let Some(ref args) = args {
                    for arg in args {
                        cmd.arg(arg);
                    }
                }
                if let Some(extras) = extra_args {
                    for arg in extras {
                        cmd.arg(arg);
                    }
                }

                #[cfg(target_os = "linux")]
                clean_appimage_env(&mut cmd);

                cmd.spawn().map_err(|e| format!("Startup failed: {}", e))?;
            }

            crate::modules::logger::log_info(&format!(
                "Antigravity startup command sent (manual path: {}, args: {:?})",
                path_str, args
            ));
            return Ok(());
        } else {
            crate::modules::logger::log_warn(&format!(
                "Manual configuration path does not exist: {}, falling back to auto-detection",
                path_str
            ));
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Improvement: Use output() to wait for open command completion and capture "app not found" error
        let mut cmd = Command::new("open");
        prepare_launch_stdio(&mut cmd, detached)?;
        let app_name = if target_ide == Some("ide") {
            "Antigravity IDE"
        } else {
            "Antigravity"
        };
        cmd.args(["-a", app_name]);

        // Add startup arguments
        if let Some(ref args) = args {
            for arg in args {
                cmd.arg(arg);
            }
        }
        if let Some(extras) = extra_args {
            for arg in extras {
                cmd.arg(arg);
            }
        }

        let output = cmd
            .output()
            .map_err(|e| format!("Execute open command failed: {}", e))?;
        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Startup failed: {}", err_msg.trim()));
        }

        crate::modules::logger::log_info("Antigravity startup command sent (macOS open)");
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Windows/Linux Auto-detection and Startup
        if let Some(detected_path) = get_antigravity_executable_path(target_ide) {
            let mut cmd = Command::new(&detected_path);
            if let Some(parent) = detected_path.parent() {
                cmd.current_dir(parent);
            }
            prepare_launch_stdio(&mut cmd, detached)?;

            // Add startup arguments
            if let Some(ref args) = args {
                for arg in args {
                    cmd.arg(arg);
                }
            }
            if let Some(extras) = extra_args {
                for arg in extras {
                    cmd.arg(arg);
                }
            }

            #[cfg(target_os = "linux")]
            clean_appimage_env(&mut cmd);

            cmd.spawn().map_err(|e| {
                format!("Startup failed (detected path {:?}): {}", detected_path, e)
            })?;

            crate::modules::logger::log_info(&format!(
                "Antigravity startup command sent (detected path: {:?})",
                detected_path
            ));
            Ok(())
        } else {
            Err("Unable to start Antigravity: executable not found".to_string())
        }
    }
}

fn get_process_info(target_ide: Option<&str>) -> (Option<std::path::PathBuf>, Option<Vec<String>>) {
    let mut system = System::new_all();
    system.refresh_all();

    let current_exe = get_current_exe_path();
    let current_pid = std::process::id();

    for (pid, process) in system.processes() {
        let pid_u32 = pid.as_u32();
        if pid_u32 == current_pid {
            continue;
        }

        // Exclude manager process itself
        if let (Some(ref my_path), Some(p_exe)) = (&current_exe, process.exe()) {
            if let Ok(p_path) = p_exe.canonicalize() {
                if my_path == &p_path {
                    continue;
                }
            }
        }

        let name = process.name().to_string_lossy().to_lowercase();
        if let Some(exe) = process.exe() {
            if is_manager_process(&name, &exe.to_string_lossy()) {
                continue;
            }
        }

        // Get executable path and command line arguments
        if let Some(exe) = process.exe() {
            let mut args = process.cmd().iter();
            let exe_path = args
                .next()
                .map_or(exe.to_string_lossy(), |arg| arg.to_string_lossy())
                .to_lowercase();

            // Keep the original argument case. Matching uses a lowercase copy.
            let args = args
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<String>>();

            let args_str = args.join(" ").to_lowercase();

            // Common helper process exclusion logic
            let is_helper = is_helper_process(&name, &args_str, &exe_path);

            let path = Some(exe.to_path_buf());
            let args = Some(args);

            // Is the process a match for target_ide?
            let is_ide_match = if target_ide == Some("ide") {
                exe_path.contains("antigravity ide")
                    || exe_path.contains("antigravity-ide")
                    || name.contains("antigravity ide")
                    || name.contains("antigravity-ide")
            } else {
                (exe_path.contains("antigravity") || name.contains("antigravity"))
                    && !exe_path.contains("antigravity ide")
                    && !exe_path.contains("antigravity-ide")
                    && !name.contains("antigravity ide")
                    && !name.contains("antigravity-ide")
            };

            if is_ide_match && !is_helper {
                #[cfg(target_os = "macos")]
                {
                    if !exe_path.contains("frameworks") {
                        if let Some(app_idx) = exe_path.find(".app") {
                            let app_path_str = &exe.to_string_lossy()[..app_idx + 4];
                            let path = Some(std::path::PathBuf::from(app_path_str));
                            return (path, args);
                        }
                    }
                    return (path, args);
                }

                #[cfg(target_os = "windows")]
                {
                    return (path, args);
                }

                #[cfg(target_os = "linux")]
                {
                    return (path, args);
                }
            }
        }
    }
    (None, None)
}

/// Get Antigravity executable path from running processes
///
/// Most reliable method to find installation anywhere
pub fn get_path_from_running_process(target_ide: Option<&str>) -> Option<std::path::PathBuf> {
    let (path, _) = get_process_info(target_ide);
    path
}

/// Get Antigravity startup arguments from running processes
pub fn get_args_from_running_process(target_ide: Option<&str>) -> Option<Vec<String>> {
    let (_, args) = get_process_info(target_ide);
    args
}

/// Get --user-data-dir argument value (if exists)
pub fn get_user_data_dir_from_process(target_ide: Option<&str>) -> Option<std::path::PathBuf> {
    // Prefer getting startup arguments from config
    if let Ok(config) = crate::modules::config::load_app_config() {
        if let Some(args) = config.antigravity_args {
            // Check arguments in config
            for i in 0..args.len() {
                if args[i] == "--user-data-dir" && i + 1 < args.len() {
                    // Next argument is the path
                    let path = std::path::PathBuf::from(&args[i + 1]);
                    if path.exists() {
                        return Some(path);
                    }
                } else if args[i].starts_with("--user-data-dir=") {
                    // Argument and value in same string, e.g. --user-data-dir=/path/to/data
                    let parts: Vec<&str> = args[i].splitn(2, '=').collect();
                    if parts.len() == 2 {
                        let path_str = parts[1];
                        let path = std::path::PathBuf::from(path_str);
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    // If not in config, get arguments from running process
    if let Some(args) = get_args_from_running_process(target_ide) {
        for i in 0..args.len() {
            if args[i] == "--user-data-dir" && i + 1 < args.len() {
                // Next argument is the path
                let path = std::path::PathBuf::from(&args[i + 1]);
                if path.exists() {
                    return Some(path);
                }
            } else if args[i].starts_with("--user-data-dir=") {
                // Argument and value in same string, e.g. --user-data-dir=/path/to/data
                let parts: Vec<&str> = args[i].splitn(2, '=').collect();
                if parts.len() == 2 {
                    let path_str = parts[1];
                    let path = std::path::PathBuf::from(path_str);
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

/// Get Antigravity executable path (cross-platform)
///
/// Search strategy (highest to lowest priority):
/// 1. Get path from running process (most reliable, supports any location)
/// 2. Iterate standard installation locations
/// 3. Return None
pub fn get_antigravity_executable_path(target_ide: Option<&str>) -> Option<std::path::PathBuf> {
    // Strategy 1: Get from running process (supports any location)
    if let Some(path) = get_path_from_running_process(target_ide) {
        return Some(path);
    }

    // Strategy 2: Check config paths (supports user-configured locations)
    if let Ok(config) = crate::modules::config::load_app_config() {
        match target_ide {
            Some("ide") => {
                if let Some(ref p) = config.antigravity_ide_executable {
                    let path = std::path::PathBuf::from(p);
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
            _ => {
                // Try antigravity_executable first (closest match for target_ide=None)
                if let Some(ref p) = config.antigravity_executable {
                    let path = std::path::PathBuf::from(p);
                    if path.exists() {
                        return Some(path);
                    }
                }
            }
        }
    }

    // Strategy 3: Check standard installation locations
    check_standard_locations(target_ide)
}

/// Check standard installation locations
fn check_standard_locations(target_ide: Option<&str>) -> Option<std::path::PathBuf> {
    let folder_names: &[&str] = if target_ide == Some("ide") {
        &["Antigravity IDE"]
    } else if target_ide == Some("code") || target_ide == Some("cursor") {
        &["Antigravity"]
    } else {
        &["Antigravity"]
    };

    #[cfg(target_os = "macos")]
    {
        for folder_name in folder_names {
            let path = std::path::PathBuf::from(format!("/Applications/{}.app", folder_name));
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::env;

        // Get environment variables
        let local_appdata = env::var("LOCALAPPDATA").ok();
        let program_files =
            env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".to_string());
        let program_files_x86 =
            env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".to_string());

        for folder_name in folder_names {
            let mut possible_paths = Vec::new();

            // User installation location (preferred)
            if let Some(local) = &local_appdata {
                possible_paths.push(
                    std::path::PathBuf::from(local)
                        .join("Programs")
                        .join(folder_name)
                        .join(format!("{}.exe", folder_name)),
                );
            }

            // System installation location
            possible_paths.push(
                std::path::PathBuf::from(&program_files)
                    .join(folder_name)
                    .join(format!("{}.exe", folder_name)),
            );

            // 32-bit compatibility location
            possible_paths.push(
                std::path::PathBuf::from(&program_files_x86)
                    .join(folder_name)
                    .join(format!("{}.exe", folder_name)),
            );

            // Return the first existing path
            for path in possible_paths {
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for folder_name in folder_names {
            let exe_name = if *folder_name == "Antigravity IDE" {
                "antigravity-ide"
            } else {
                "antigravity"
            };

            if let Some(path) = crate::modules::linux_paths::find_executable(exe_name) {
                return Some(path);
            }

            let possible_paths = vec![
                std::path::PathBuf::from(format!("/usr/bin/{}", exe_name)),
                std::path::PathBuf::from(format!("/usr/local/bin/{}", exe_name)),
                std::path::PathBuf::from(format!("/opt/{}/{}", exe_name, exe_name)),
                std::path::PathBuf::from(format!("/usr/share/{}/{}", exe_name, exe_name)),
                std::path::PathBuf::from(format!("/opt/{}/{}", folder_name, exe_name)),
                std::path::PathBuf::from(format!("/usr/share/{}/{}", folder_name, exe_name)),
            ];

            // User local installation
            if let Some(home) = dirs::home_dir() {
                let user_local = home.join(format!(".local/bin/{}", exe_name));
                if let Some(path) = crate::modules::linux_paths::executable(&user_local) {
                    return Some(path);
                }
            }

            for path in possible_paths {
                if let Some(resolved) = crate::modules::linux_paths::executable(&path) {
                    return Some(resolved);
                }
            }
        }
    }

    None
}
