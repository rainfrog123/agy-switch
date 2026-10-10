use crate::modules::{cli_credentials, db, device, process};
use std::fs;
#[cfg(target_os = "macos")]
use std::process::Command;

pub trait SystemIntegration: Send + Sync {
    /// 当切换账号时执行的系统层操作（如杀进程、写入文件、注入数据库）
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String>;

    /// The CLI overrides only launch I/O; GUI launch behavior stays unchanged.
    #[allow(dead_code)]
    fn start_application(&self, target_ide: Option<&str>) -> Result<(), String> {
        process::launch_antigravity_9222(target_ide)
    }

    /// 更新系统托盘（如果适用）
    fn update_tray(&self);

    /// 发送系统通知
    fn show_notification(&self, title: &str, body: &str);
}

/// 桌面版实现：包含完整的进程控制 and UI 同步
pub struct DesktopIntegration {
    pub app_handle: tauri::AppHandle,
}

impl SystemIntegration for DesktopIntegration {
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        let integration = DesktopIntegration {
            app_handle: self.app_handle.clone(),
        };
        let account = account.clone();
        let target = target_ide.map(str::to_owned);
        tokio::task::spawn_blocking(move || {
            DesktopIntegration::switch_sync(&integration, &account, target.as_deref())
        })
        .await
        .map_err(|_| "Account switch worker failed.".to_string())?
    }

    fn update_tray(&self) {
        let _ = crate::modules::tray::update_tray_menus(&self.app_handle);
    }

    fn show_notification(&self, title: &str, body: &str) {
        // 使用 tauri-plugin-dialog 或原生通知（此处简化）
        crate::modules::logger::log_info(&format!("[Notification] {}: {}", title, body));
    }
}

impl DesktopIntegration {
    /// Shared credential/process operations; callbacks may be headless (CLI).
    pub(crate) fn switch_sync(
        integration: &impl SystemIntegration,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        crate::modules::logger::log_info(&format!(
            "[Desktop] Executing system switch for: {} (target_ide: {:?})",
            account.email, target_ide
        ));

        #[cfg(target_os = "linux")]
        let cli_only = target_ide == Some("agy")
            || (target_ide.is_none()
                && process::get_antigravity_executable_path(target_ide).is_none()
                && crate::modules::linux_paths::find_executable("agy").is_some()
                && cli_session_path()?.is_some());
        #[cfg(not(target_os = "linux"))]
        let cli_only = target_ide == Some("agy");

        if cli_only {
            write_to_system_keyring(account, true)?;

        if let Ok(storage_path) = device::get_storage_path(target_ide) {
            if let Some(ref profile) = account.device_profile {
                let _ = device::write_profile(&storage_path, profile);
                let _ = device::rewrite_install_identifiers(profile);
            }
        }
            let _ = device::sync_desktop_app_storage(account);

            let is_running = process::is_process_running_by_name("agy");
            let msg = if is_running {
                format!(
                    "Account {} activated. Agy is running; start a new CLI command to use the updated session.",
                    account.email
                )
            } else {
                format!(
                    "Account {} activated. Token is ready for your next CLI command.",
                    account.email
                )
            };
            integration.show_notification("Antigravity CLI", &msg);
            integration.update_tray();

            return Ok(());
        }

        #[cfg(target_os = "linux")]
        if process::get_antigravity_executable_path(target_ide).is_none() {
            return Err("Cannot find Antigravity. Set its executable path in Settings, or install and initialize agy before switching accounts.".into());
        }

        // 0. 在关闭外部进程前，预先快照捕获正在运行的可执行文件路径与启动参数
        let active_exe_path = process::get_antigravity_executable_path(target_ide);
        let active_args = process::get_args_from_running_process(target_ide);

        // 1. 完全关闭 Antigravity 并等待 2 秒（释放全部锁与资源，清除 orphan language_server）
        let is_ide_mode = target_ide == Some("ide");
        let is_app_cli_mode = target_ide == Some("app");
        let is_vscode_mode = target_ide == Some("vscode");

        if is_vscode_mode {
            // VS Code 插件模式：不退出 VS Code 宿主，直接进行凭据热更新
        } else {
            process::fully_close_antigravity(target_ide)?;
        }

        // 2. 凭据全域同步写入（Keychain + CLI + VS Code 插件文件 + SQLite 数据库）
        // 2.1 写入系统 Keychain (macOS / Windows / Linux) 与 CLI 会话
        if !is_ide_mode && !is_vscode_mode {
            write_to_system_keyring(account, false)?;
        }

        // 2.2 写入多端凭据文件 (~/.gemini/)
        write_to_file_credentials(account)?;

        if let Ok(storage_path) = device::get_storage_path(target_ide) {
            if let Some(ref profile) = account.device_profile {
                let _ = device::write_profile(&storage_path, profile);
                let _ = device::rewrite_install_identifiers(profile);
            }
        }
        let _ = device::sync_desktop_app_storage(account);

        // 2.4 注入对应 state.vscdb。这个安装没有该文件；只写确实属于目标编辑器的库。
        let candidate_dbs = if is_app_cli_mode {
            let mut paths = Vec::new();
            #[cfg(target_os = "macos")]
            if let Some(home) = dirs::home_dir() {
                paths.push(home.join("Library/Application Support/Antigravity/User/globalStorage/state.vscdb"));
            }
            #[cfg(target_os = "windows")]
            if let Ok(appdata) = std::env::var("APPDATA") {
                paths.push(std::path::PathBuf::from(appdata).join("Antigravity\\User\\globalStorage\\state.vscdb"));
            }
            #[cfg(target_os = "linux")]
            if let Some(config_home) = crate::modules::linux_paths::config_home() {
                paths.push(config_home.join("Antigravity/User/globalStorage/state.vscdb"));
            }
            paths
        } else {
            db::get_all_candidate_db_paths(target_ide)
        };
        let mut injected = 0usize;
        for db_path in candidate_dbs {
            if !db_path.exists() || !db_matches_switch_target(&db_path, target_ide) {
                continue;
            }
            let backup_path = db_path.with_extension("vscdb.backup");
            let _ = fs::copy(&db_path, &backup_path);
            db::inject_token(
                &db_path,
                &account.token.access_token,
                &account.token.refresh_token,
                account.token.expiry_timestamp,
                &account.email,
                account.token.is_gcp_tos,
                account.token.project_id.as_deref(),
                account.token.id_token.as_deref(),
                account.token.oauth_client_key.as_deref(),
            )
            .map_err(|err| format!("Failed to write Antigravity session database {:?}: {err}", db_path))?;
            if let Some(ref profile) = account.device_profile {
                let _ = db::write_service_machine_id(
                    &db_path,
                    &profile.dev_device_id,
                    &profile.mac_machine_id,
                );
            }
            injected += 1;
        }
        if injected == 0 && !is_vscode_mode {
            crate::modules::logger::log_warn(
                "No Antigravity state.vscdb found. Session was written to the credential store and ~/.gemini token files.",
            );
        }

        // 3. 在 9222 远程调试端口下重新拉起 Antigravity (利用预快照的 exe 和 args)
        if is_vscode_mode {
            // 免重启宿主编辑器
        } else {
            process::launch_antigravity_9222_with_fallback(
                target_ide,
                active_exe_path.as_deref(),
                active_args.as_deref(),
            )?;
        }

        // 4. 更新托盘
        integration.update_tray();

        Ok(())
    }
}

pub(crate) fn db_matches_switch_target(path: &std::path::Path, target_ide: Option<&str>) -> bool {
    let text = path.to_string_lossy().to_lowercase().replace('/', "\\");
    let ide = text.contains("\\antigravity ide\\") || text.contains("\\antigravity-ide\\");
    let code = text.contains("\\code\\") || text.contains("\\code - insiders\\");
    let cursor = text.contains("\\cursor\\");
    match target_ide {
        Some("ide") => ide,
        Some("vscode") | Some("code") => code,
        Some("cursor") => cursor,
        _ => text.contains("\\antigravity\\") && !ide && !code && !cursor,
    }
}

/// 同步写入本地文件凭据 (~/.gemini/ 目录下的所有工具凭据文件)
pub(crate) fn write_to_file_credentials(account: &crate::models::Account) -> Result<(), String> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Err("Failed to resolve user home directory".to_string()),
    };
    let gemini_dir = home.join(".gemini");

    if !gemini_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&gemini_dir) {
            crate::modules::logger::log_warn(&format!(
                "[Desktop] Failed to create .gemini directory: {}",
                e
            ));
            return Err(format!("Failed to create .gemini directory: {}", e));
        }
    }

    // 1. 同步写入 jetski-standalone-oauth-token (VS Code 插件专用)
    let jetski_file = gemini_dir.join("jetski-standalone-oauth-token");
    if let Ok(payload) = cli_credentials::payload(&account.token) {
        let _ = cli_credentials::write_session(&jetski_file, &payload);
    }

    // 2. 同步写入 Antigravity CLI (agy) 原生文件凭据: ~/.gemini/antigravity-cli/antigravity-oauth-token
    let agy_cli_dir = gemini_dir.join("antigravity-cli");
    if !agy_cli_dir.exists() {
        let _ = std::fs::create_dir_all(&agy_cli_dir);
    }

    let expiry_secs = if account.token.expiry_timestamp > 10_000_000_000 {
        account.token.expiry_timestamp / 1000
    } else {
        account.token.expiry_timestamp
    };
    let expiry_datetime =
        chrono::DateTime::from_timestamp(expiry_secs, 0).unwrap_or_else(|| chrono::Utc::now());
    let expiry_str = expiry_datetime.to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    #[derive(serde::Serialize)]
    struct AgyTokenDetails {
        access_token: String,
        token_type: String,
        refresh_token: String,
        expiry: String,
    }

    #[derive(serde::Serialize)]
    struct AgyOAuthTokenFile {
        token: AgyTokenDetails,
        auth_method: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id_token: Option<String>,
    }

    let agy_token = AgyOAuthTokenFile {
        token: AgyTokenDetails {
            access_token: account.token.access_token.clone(),
            token_type: "Bearer".to_string(),
            refresh_token: account.token.refresh_token.clone(),
            expiry: expiry_str,
        },
        auth_method: "consumer".to_string(),
        id_token: account.token.id_token.clone(),
    };

    let agy_token_path = agy_cli_dir.join("antigravity-oauth-token");
    if let Ok(agy_json_str) = serde_json::to_string_pretty(&agy_token) {
        let _ = std::fs::write(&agy_token_path, agy_json_str);
    }

    // 3. 同步写入 Gemini CLI 凭据 (~/.gemini/oauth_creds.json 以及 ~/.gemini/google_accounts.json)
    let expiry_ms = if account.token.expiry_timestamp > 10_000_000_000 {
        account.token.expiry_timestamp
    } else {
        account.token.expiry_timestamp * 1000
    };

    #[derive(serde::Serialize)]
    struct OAuthCredsFile {
        access_token: String,
        refresh_token: String,
        token_type: String,
        expiry_date: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        id_token: Option<String>,
        scope: String,
    }

    let creds = OAuthCredsFile {
        access_token: account.token.access_token.clone(),
        refresh_token: account.token.refresh_token.clone(),
        token_type: "Bearer".to_string(),
        expiry_date: expiry_ms,
        id_token: account.token.id_token.clone(),
        scope: "https://www.googleapis.com/auth/userinfo.email openid https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.profile".to_string(),
    };

    let creds_path = gemini_dir.join("oauth_creds.json");
    if let Ok(json_str) = serde_json::to_string_pretty(&creds) {
        let _ = std::fs::write(&creds_path, json_str);
    }

    #[derive(serde::Serialize)]
    struct GoogleAccountsFile {
        active: String,
        old: Vec<String>,
    }

    let accounts_info = GoogleAccountsFile {
        active: account.email.clone(),
        old: vec![],
    };

    let accounts_path = gemini_dir.join("google_accounts.json");
    if let Ok(accounts_json_str) = serde_json::to_string_pretty(&accounts_info) {
        let _ = std::fs::write(&accounts_path, accounts_json_str);
    }

    crate::modules::logger::log_info(&format!(
        "[Desktop] Successfully synced all file-based credentials in ~/.gemini for: {}",
        account.email
    ));

    Ok(())
}

fn cli_session_path() -> Result<Option<std::path::PathBuf>, String> {
    let home = dirs::home_dir().ok_or("Failed to resolve user home directory")?;
    cli_credentials::session_path(&home)
}

pub fn read_cli_credentials() -> Result<crate::modules::migration::ImportedOAuthState, String> {
    let path = cli_session_path()?.ok_or("No initialized agy CLI session found.")?;
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("Cannot read agy session: {error}"))?;
    parse_keyring_payload(&payload)
}

/// Sync the APP's platform credential store and an existing native agy session.
pub(crate) fn write_to_system_keyring(
    account: &crate::models::Account,
    cli_only: bool,
) -> Result<(), String> {
    let payload_json = cli_credentials::payload(&account.token)?;
    let cli_path = cli_session_path()?;

    // Explicit CLI switches do not need or modify the APP credential store.
    // Require initialization so a missing session cannot be reported as success.
    if cli_only {
        return cli_credentials::sync_session(cli_path.as_deref(), &payload_json, true);
    }

    crate::modules::logger::log_info(&format!(
        "[Desktop] Writing token to system credential store for: {}",
        account.email
    ));

    // 2. 跨平台凭据注入
    #[cfg(target_os = "macos")]
    {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let encoded_payload = STANDARD.encode(&payload_json);
        let full_keyring_value = format!("go-keyring-base64:{}", encoded_payload);

        // 2.1 macOS Keychain Access
        // 删除旧的
        let _ = Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
            ])
            .output();

        // 写入新的 (-A 参数允许所有本地应用免密码、无感直接读取凭据)
        let output = Command::new("security")
            .args([
                "add-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
                "-w",
                &full_keyring_value,
                "-A",
            ])
            .output()
            .map_err(|e| format!("Failed to execute security command: {}", e))?;

        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            return Err(format!("macOS security command failed: {}", err_msg.trim()));
        }
    }

    #[cfg(target_os = "windows")]
    {
        // 2.2 Windows Credential Manager direct Win32 API calls to write raw UTF-8 bytes
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        #[repr(C)]
        struct FILETIME {
            dw_low_date_time: u32,
            dw_high_date_time: u32,
        }

        #[repr(C)]
        struct CREDENTIALW {
            flags: u32,
            cred_type: u32,
            target_name: *const u16,
            comment: *const u16,
            last_written: FILETIME,
            credential_blob_size: u32,
            credential_blob: *const u8,
            persist: u32,
            attribute_count: u32,
            attributes: *const std::ffi::c_void,
            target_alias: *const u16,
            user_name: *const u16,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn CredWriteW(credential: *const CREDENTIALW, flags: u32) -> i32;
            fn CredDeleteW(target_name: *const u16, type_: u32, flags: u32) -> i32;
        }

        let target = "gemini:antigravity";
        let user = "antigravity";
        let secret = payload_json.as_bytes();

        let target_wide: Vec<u16> = std::ffi::OsStr::new(target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let user_wide: Vec<u16> = std::ffi::OsStr::new(user)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let cred = CREDENTIALW {
            flags: 0,
            cred_type: 1, // CRED_TYPE_GENERIC
            target_name: target_wide.as_ptr(),
            comment: ptr::null(),
            last_written: FILETIME {
                dw_low_date_time: 0,
                dw_high_date_time: 0,
            },
            credential_blob_size: secret.len() as u32,
            credential_blob: secret.as_ptr(),
            persist: 2, // CRED_PERSIST_LOCAL_MACHINE
            attribute_count: 0,
            attributes: ptr::null(),
            target_alias: ptr::null(),
            user_name: user_wide.as_ptr(),
        };

        unsafe {
            // Delete first to ensure we write clean
            let _ = CredDeleteW(target_wide.as_ptr(), 1, 0);

            let res = CredWriteW(&cred, 0);
            if res == 0 {
                let err = std::io::Error::last_os_error();
                return Err(format!("Windows CredWriteW failed: {}", err));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(path) = cli_path {
            let cli_payload = payload_json.clone();
            crate::modules::linux_credentials::write_with_commit(&payload_json, move || {
                cli_credentials::write_session(&path, &cli_payload)
            })?;
        } else {
            crate::modules::linux_credentials::write(&payload_json)?;
        }
    }

    #[cfg(not(target_os = "linux"))]
    cli_credentials::sync_session(cli_path.as_deref(), &payload_json, false).map_err(|error| {
        format!("System credential store was updated, but agy session sync failed: {error}")
    })?;

    crate::modules::logger::log_info(
        "[Desktop] Successfully wrote token to system credential store.",
    );

    Ok(())
}

/// 辅助方法：从宿主操作系统的 Keychain/Credentials Manager 读取 Token
pub fn read_from_system_keyring() -> Result<crate::modules::migration::ImportedOAuthState, String> {
    #[cfg(target_os = "macos")]
    {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "gemini",
                "-a",
                "antigravity",
                "-w",
            ])
            .output()
            .map_err(|e| format!("Failed to execute security command: {}", e))?;

        if !output.status.success() {
            return Err("No credential found in macOS Keychain".to_string());
        }

        let secret_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let payload_str = if secret_str.starts_with("go-keyring-base64:") {
            let b64_part = &secret_str["go-keyring-base64:".len()..];
            let decoded = STANDARD
                .decode(b64_part)
                .map_err(|e| format!("Base64 decode failed: {}", e))?;
            String::from_utf8(decoded).map_err(|e| format!("UTF-8 decode failed: {}", e))?
        } else {
            secret_str
        };

        return parse_keyring_payload(&payload_str);
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;

        #[repr(C)]
        struct FILETIME {
            dw_low_date_time: u32,
            dw_high_date_time: u32,
        }

        #[repr(C)]
        struct CREDENTIALW {
            flags: u32,
            cred_type: u32,
            target_name: *const u16,
            comment: *const u16,
            last_written: FILETIME,
            credential_blob_size: u32,
            credential_blob: *mut u8,
            persist: u32,
            attribute_count: u32,
            attributes: *const std::ffi::c_void,
            target_alias: *const u16,
            user_name: *const u16,
        }

        #[link(name = "advapi32")]
        extern "system" {
            fn CredReadW(
                target_name: *const u16,
                type_: u32,
                flags: u32,
                credential: *mut *mut CREDENTIALW,
            ) -> i32;
            fn CredFree(buffer: *mut std::ffi::c_void);
        }

        let target = "gemini:antigravity";
        let target_wide: Vec<u16> = std::ffi::OsStr::new(target)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut cred_ptr: *mut CREDENTIALW = ptr::null_mut();
        unsafe {
            let res = CredReadW(target_wide.as_ptr(), 1, 0, &mut cred_ptr);
            if res == 0 || cred_ptr.is_null() {
                return Err("No credential found in Windows Credential Manager".to_string());
            }

            let cred = &*cred_ptr;
            let blob = std::slice::from_raw_parts(
                cred.credential_blob,
                cred.credential_blob_size as usize,
            );
            let payload_str = String::from_utf8_lossy(blob).to_string();
            CredFree(cred_ptr as *mut std::ffi::c_void);

            return parse_keyring_payload(&payload_str);
        }
    }

    #[cfg(target_os = "linux")]
    {
        return parse_keyring_payload(&crate::modules::linux_credentials::read()?);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("Keyring not supported on this operating system".to_string())
    }
}

fn parse_keyring_payload(
    payload_str: &str,
) -> Result<crate::modules::migration::ImportedOAuthState, String> {
    let json: serde_json::Value = serde_json::from_str(payload_str)
        .map_err(|e| format!("Failed to parse keyring payload JSON: {}", e))?;

    let refresh_token = json
        .get("token")
        .and_then(|t| t.get("refresh_token"))
        .and_then(|v| v.as_str())
        .or_else(|| json.get("refresh_token").and_then(|v| v.as_str()))
        .ok_or_else(|| "Refresh Token not found in keyring payload".to_string())?
        .to_string();

    Ok(crate::modules::migration::ImportedOAuthState {
        refresh_token,
        is_gcp_tos: true,
        project_id: None,
    })
}

/// Desktop integration manager used by account and OAuth services.
#[derive(Clone)]
pub struct SystemManager {
    pub app_handle: tauri::AppHandle,
}

impl SystemManager {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }

    pub fn update_tray(&self) {
        DesktopIntegration {
            app_handle: self.app_handle.clone(),
        }
        .update_tray();
    }
}

impl SystemIntegration for SystemManager {
    async fn on_account_switch(
        &self,
        account: &crate::models::Account,
        target_ide: Option<&str>,
    ) -> Result<(), String> {
        DesktopIntegration {
            app_handle: self.app_handle.clone(),
        }
        .on_account_switch(account, target_ide)
        .await
    }

    fn update_tray(&self) {
        SystemManager::update_tray(self);
    }

    fn show_notification(&self, title: &str, body: &str) {
        DesktopIntegration {
            app_handle: self.app_handle.clone(),
        }
        .show_notification(title, body);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    #[ignore = "requires a disposable D-Bus/keyring session"]
    fn isolated_linux_app_and_cli_sync() {
        assert_eq!(
            std::env::var("ANTIGRAVITY_TEST_SECRET_SERVICE").as_deref(),
            Ok("isolated")
        );
        let home = dirs::home_dir().unwrap();
        std::fs::create_dir_all(home.join(".gemini/antigravity-cli")).unwrap();
        let path = cli_session_path().unwrap().unwrap();
        let mut account = crate::models::Account::new(
            "fixture".into(),
            "fixture@example.invalid".into(),
            crate::models::TokenData::new(
                "fixture-access".into(),
                "fixture-app".into(),
                3600,
                None,
                None,
                None,
                true,
                Some("fixture-id".into()),
            ),
        );
        write_to_system_keyring(&account, false).unwrap();
        let app_payload = crate::modules::linux_credentials::read().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), app_payload);

        // An explicit CLI switch must leave the APP's active credential alone.
        account.token.refresh_token = "fixture-cli-only".into();
        write_to_system_keyring(&account, true).unwrap();
        assert_eq!(
            read_cli_credentials().unwrap().refresh_token,
            "fixture-cli-only"
        );
        assert_eq!(
            crate::modules::linux_credentials::read().unwrap(),
            app_payload
        );

        // Force the real APP+CLI commit callback to fail and verify rollback.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let failure = write_to_system_keyring(&account, false).unwrap_err();
        assert!(failure.contains("Previous credentials were restored"));
        assert_eq!(
            crate::modules::linux_credentials::read().unwrap(),
            app_payload
        );
        assert!(path.is_dir());
        std::fs::remove_dir(&path).unwrap();
    }

    #[test]
    #[ignore = "requires a disposable HOME; run with --test-threads=1"]
    fn isolated_linux_cli_switch() {
        assert_eq!(
            std::env::var("ANTIGRAVITY_TEST_SECRET_SERVICE").as_deref(),
            Ok("isolated")
        );
        let home = dirs::home_dir().unwrap();
        let bin = home.join(".local/bin");
        let session = home.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&session).unwrap();
        let executable = bin.join("agy");
        std::fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let unrelated = home.join(".gemini/oauth_creds.json");
        let unrelated_accounts = home.join(".gemini/google_accounts.json");
        std::fs::write(&unrelated, b"unrelated-gemini-fixture").unwrap();
        std::fs::write(&unrelated_accounts, b"unrelated-accounts-fixture").unwrap();
        let mut account = crate::models::Account::new(
            "fixture".into(),
            "fixture@example.invalid".into(),
            crate::models::TokenData::new(
                "fixture-access".into(),
                "fixture-old".into(),
                3600,
                None,
                None,
                None,
                true,
                None,
            ),
        );
        write_to_system_keyring(&account, true).unwrap();
        assert_eq!(read_cli_credentials().unwrap().refresh_token, "fixture-old");
        account.token.refresh_token = "fixture-new".into();
        write_to_system_keyring(&account, true).unwrap();
        assert_eq!(read_cli_credentials().unwrap().refresh_token, "fixture-new");
        let metadata = std::fs::metadata(session.join("antigravity-oauth-token")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::read(unrelated).unwrap(),
            b"unrelated-gemini-fixture"
        );
        assert_eq!(
            std::fs::read(unrelated_accounts).unwrap(),
            b"unrelated-accounts-fixture"
        );
    }
}
