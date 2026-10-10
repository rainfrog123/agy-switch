use crate::models::DeviceProfile;
use crate::modules::{logger, process};
use chrono::Local;
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const GLOBAL_BASELINE: &str = "device_original.json";

fn get_data_dir() -> Result<PathBuf, String> {
    crate::modules::account::get_data_dir()
}

/// Find storage.json path (prefer custom/portable paths)
pub fn get_storage_path(target_ide: Option<&str>) -> Result<PathBuf, String> {
    // 1) --user-data-dir flag
    if let Some(user_data_dir) = process::get_user_data_dir_from_process(target_ide) {
        let path = user_data_dir
            .join("User")
            .join("globalStorage")
            .join("storage.json");
        if path.exists() {
            return Ok(path);
        }
    }

    // 2) Portable mode (based on executable data/user-data)
    if let Some(exe_path) = process::get_antigravity_executable_path(target_ide) {
        if let Some(parent) = exe_path.parent() {
            let portable = parent
                .join("data")
                .join("user-data")
                .join("User")
                .join("globalStorage")
                .join("storage.json");
            if portable.exists() {
                return Ok(portable);
            }
        }
    }

    let folder_names: &[&str] = if target_ide == Some("ide") {
        &["Antigravity IDE"]
    } else if target_ide == Some("code") || target_ide == Some("cursor") {
        &["Antigravity"]
    } else {
        // target_ide = None: try IDE folder first, fall back to classic name
        &["Antigravity IDE", "Antigravity"]
    };

    // 3) Standard installation location
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or("failed_to_get_home_dir")?;
        for folder_name in folder_names {
            let path = home.join(format!(
                "Library/Application Support/{}/User/globalStorage/storage.json",
                folder_name
            ));
            if path.exists() {
                return Ok(path);
            }
        }
        for folder_name in folder_names {
            let base_dir = home.join(format!("Library/Application Support/{}", folder_name));
            if base_dir.exists() {
                let path = base_dir.join("User/globalStorage/storage.json");
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if !path.exists() {
                    let _ = fs::write(&path, "{\"telemetry\":{}}");
                }
                return Ok(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let appdata =
            std::env::var("APPDATA").map_err(|_| "failed_to_get_appdata_env".to_string())?;
        for folder_name in folder_names {
            let path = PathBuf::from(&appdata)
                .join(folder_name)
                .join("User\\globalStorage\\storage.json");
            if path.exists() {
                return Ok(path);
            }
        }
        for folder_name in folder_names {
            let base_dir = PathBuf::from(&appdata).join(folder_name);
            if base_dir.exists() {
                let path = base_dir.join("User\\globalStorage\\storage.json");
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if !path.exists() {
                    let _ = fs::write(&path, "{\"telemetry\":{}}");
                }
                return Ok(path);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let config_home =
            crate::modules::linux_paths::config_home().ok_or("failed_to_get_config_dir")?;
        for folder_name in folder_names {
            let path = config_home
                .join(folder_name)
                .join("User/globalStorage/storage.json");
            if path.exists() {
                return Ok(path);
            }
        }
        for folder_name in folder_names {
            let base_dir = config_home.join(folder_name);
            if base_dir.exists() {
                let path = base_dir.join("User/globalStorage/storage.json");
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if !path.exists() {
                    let _ = fs::write(&path, "{\"telemetry\":{}}");
                }
                return Ok(path);
            }
        }
    }

    Err("storage_json_not_found".to_string())
}

/// Get directory of storage.json
#[allow(dead_code)]
pub fn get_storage_dir() -> Result<PathBuf, String> {
    let path = get_storage_path(None)?;
    path.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "failed_to_get_storage_parent_dir".to_string())
}

/// Get state.vscdb path (same directory as storage.json)
#[allow(dead_code)]
pub fn get_state_db_path() -> Result<PathBuf, String> {
    let dir = get_storage_dir()?;
    Ok(dir.join("state.vscdb"))
}

/// Backup storage.json, returns backup file path
pub fn backup_storage(storage_path: &Path) -> Result<PathBuf, String> {
    if !storage_path.exists() {
        return Err(format!("storage_json_missing: {:?}", storage_path));
    }
    let dir = storage_path
        .parent()
        .ok_or_else(|| "failed_to_get_storage_parent_dir".to_string())?;
    let backup_path = dir.join(format!(
        "storage.json.backup_{}",
        Local::now().format("%Y%m%d_%H%M%S")
    ));
    fs::copy(storage_path, &backup_path).map_err(|e| format!("backup_failed: {}", e))?;
    Ok(backup_path)
}

/// Read current device profile from storage.json
pub fn read_profile(storage_path: &Path) -> Result<DeviceProfile, String> {
    let content = fs::read_to_string(storage_path)
        .map_err(|e| format!("read_failed ({:?}): {}", storage_path, e))?;
    let json: Value = serde_json::from_str(&content)
        .map_err(|e| format!("parse_failed ({:?}): {}", storage_path, e))?;

    let get_field = |key: &str| -> Option<String> {
        if let Some(obj) = json.get("telemetry").and_then(|v| v.as_object()) {
            if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
        if let Some(v) = json
            .get(format!("telemetry.{key}"))
            .and_then(|v| v.as_str())
        {
            return Some(v.to_string());
        }
        None
    };

    Ok(DeviceProfile {
        machine_id: get_field("machineId").ok_or("missing_machine_id")?,
        mac_machine_id: get_field("macMachineId").ok_or("missing_mac_machine_id")?,
        dev_device_id: get_field("devDeviceId").ok_or("missing_dev_device_id")?,
        sqm_id: get_field("sqmId").ok_or("missing_sqm_id")?,
    })
}

/// Load/Save global original profile (shared across all accounts)
pub fn load_global_original() -> Option<DeviceProfile> {
    if let Ok(dir) = get_data_dir() {
        let path = dir.join(GLOBAL_BASELINE);
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(profile) = serde_json::from_str::<DeviceProfile>(&content) {
                    return Some(profile);
                }
            }
        }
    }
    None
}

pub fn save_global_original(profile: &DeviceProfile) -> Result<(), String> {
    let dir = get_data_dir()?;
    let path = dir.join(GLOBAL_BASELINE);
    if path.exists() {
        return Ok(()); // already exists, don't overwrite
    }
    let content =
        serde_json::to_string_pretty(profile).map_err(|e| format!("serialize_failed: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("write_failed: {}", e))
}

/// List storage.json backups in current directory (descending by time)
#[allow(dead_code)]
pub fn list_backups(storage_path: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = storage_path
        .parent()
        .ok_or_else(|| "failed_to_get_storage_parent_dir".to_string())?;
    let mut backups = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with("storage.json.backup_") {
                    backups.push(path);
                }
            }
        }
    }
    backups.sort_by(|a, b| {
        let ma = fs::metadata(a).and_then(|m| m.modified()).ok();
        let mb = fs::metadata(b).and_then(|m| m.modified()).ok();
        mb.cmp(&ma)
    });
    Ok(backups)
}

/// Restore backup to storage.json
#[allow(dead_code)]
pub fn restore_backup(storage_path: &Path, use_oldest: bool) -> Result<PathBuf, String> {
    let backups = list_backups(storage_path)?;
    if backups.is_empty() {
        return Err("no_backups_found".to_string());
    }
    let target = if use_oldest {
        backups.last().unwrap().clone()
    } else {
        backups.first().unwrap().clone()
    };
    let _ = backup_storage(storage_path)?;
    fs::copy(&target, storage_path).map_err(|e| format!("restore_failed: {}", e))?;
    logger::log_info(&format!("storage_json_restored: {:?}", target));
    Ok(target)
}

/// Write device profile to storage.json
pub fn write_profile(storage_path: &Path, profile: &DeviceProfile) -> Result<(), String> {
    if !storage_path.exists() {
        return Err(format!("storage_json_missing: {:?}", storage_path));
    }

    // Preserve original hardware profile before first modification
    if load_global_original().is_none() {
        if let Ok(current_profile) = read_profile(storage_path) {
            let _ = save_global_original(&current_profile);
        }
    }

    // Backup current storage.json before writing
    let _ = backup_storage(storage_path);

    let content = fs::read_to_string(storage_path).map_err(|e| format!("read_failed: {}", e))?;
    let mut json: Value =
        serde_json::from_str(&content).map_err(|e| format!("parse_failed: {}", e))?;

    // Ensure telemetry is an object
    if !json.get("telemetry").map_or(false, |v| v.is_object()) {
        if json.as_object_mut().is_some() {
            json["telemetry"] = serde_json::json!({});
        } else {
            return Err("json_top_level_not_object".to_string());
        }
    }

    if let Some(telemetry) = json.get_mut("telemetry").and_then(|v| v.as_object_mut()) {
        telemetry.insert(
            "machineId".to_string(),
            Value::String(profile.machine_id.clone()),
        );
        telemetry.insert(
            "macMachineId".to_string(),
            Value::String(profile.mac_machine_id.clone()),
        );
        telemetry.insert(
            "devDeviceId".to_string(),
            Value::String(profile.dev_device_id.clone()),
        );
        telemetry.insert("sqmId".to_string(), Value::String(profile.sqm_id.clone()));
    } else {
        return Err("telemetry_not_object".to_string());
    }

    // Write flat keys as well, compatible with old formats
    if let Some(map) = json.as_object_mut() {
        map.insert(
            "telemetry.machineId".to_string(),
            Value::String(profile.machine_id.clone()),
        );
        map.insert(
            "telemetry.macMachineId".to_string(),
            Value::String(profile.mac_machine_id.clone()),
        );
        map.insert(
            "telemetry.devDeviceId".to_string(),
            Value::String(profile.dev_device_id.clone()),
        );
        map.insert(
            "telemetry.sqmId".to_string(),
            Value::String(profile.sqm_id.clone()),
        );
    }

    // Sync storage.serviceMachineId (match with devDeviceId), place at root level
    if let Some(map) = json.as_object_mut() {
        map.insert(
            "storage.serviceMachineId".to_string(),
            Value::String(profile.dev_device_id.clone()),
        );
    }

    let updated =
        serde_json::to_string_pretty(&json).map_err(|e| format!("serialize_failed: {}", e))?;
    fs::write(storage_path, updated)
        .map_err(|e| format!("write_failed ({:?}): {}", storage_path, e))?;
    logger::log_info(&format!("device_profile_written to {:?}", storage_path));

    // Sync ItemTable.storage.serviceMachineId and telemetry.serviceMachineId in state.vscdb
    let _ = sync_state_service_machine_id_value(storage_path, &profile.dev_device_id, &profile.mac_machine_id);
    Ok(())
}

fn state_db_path_for(storage_path: &Path) -> Result<PathBuf, String> {
    storage_path
        .parent()
        .map(|dir| dir.join("state.vscdb"))
        .ok_or_else(|| "failed_to_get_storage_parent_dir".to_string())
}

fn sync_state_service_machine_id_value(
    storage_path: &Path,
    service_id: &str,
    mac_machine_id: &str,
) -> Result<(), String> {
    let db_path = state_db_path_for(storage_path)?;
    if !db_path.exists() {
        logger::log_warn(&format!("state_db_missing: {:?}", db_path));
        return Ok(());
    }

    let conn = Connection::open(&db_path).map_err(|e| format!("db_open_failed: {}", e))?;
    conn.busy_timeout(std::time::Duration::from_millis(2000))
        .map_err(|e| format!("db_timeout_failed: {}", e))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT PRIMARY KEY, value TEXT);",
        [],
    )
    .map_err(|e| format!("failed_to_create_item_table: {}", e))?;
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('storage.serviceMachineId', ?1);",
        [service_id],
    )
    .map_err(|e| format!("failed_to_write_to_db: {}", e))?;
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('telemetry.serviceMachineId', ?1);",
        [mac_machine_id],
    )
    .map_err(|e| format!("failed_to_write_to_db: {}", e))?;
    logger::log_info("service_machine_id_synced_to_db");
    Ok(())
}

/// Generate a new set of device fingerprints (Cursor/VSCode style)
pub fn generate_profile() -> DeviceProfile {
    DeviceProfile {
        machine_id: format!("auth0|user_{}", random_hex(32)),
        mac_machine_id: new_standard_machine_id(),
        dev_device_id: Uuid::new_v4().to_string(),
        sqm_id: format!("{{{}}}", Uuid::new_v4().to_string().to_uppercase()),
    }
}

fn random_hex(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect::<String>()
        .to_lowercase()
}

fn new_standard_machine_id() -> String {
    // xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx (y in 8..b)
    let mut rng = rand::thread_rng();
    let mut id = String::with_capacity(36);
    for ch in "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".chars() {
        if ch == '-' || ch == '4' {
            id.push(ch);
        } else if ch == 'x' {
            id.push_str(&format!("{:x}", rng.gen_range(0..16)));
        } else if ch == 'y' {
            id.push_str(&format!("{:x}", rng.gen_range(8..12)));
        }
    }
    id
}

/// Synchronize native Antigravity Desktop app storage (app_storage.json and .updaterId)
pub fn sync_desktop_app_storage(account: &crate::models::Account) -> Result<(), String> {
    let mut candidate_dirs: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        candidate_dirs.push(PathBuf::from(&appdata).join("Antigravity"));
        candidate_dirs.push(PathBuf::from(&appdata).join("Antigravity IDE"));
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        candidate_dirs.push(home.join("Library/Application Support/Antigravity"));
        candidate_dirs.push(home.join("Library/Application Support/Antigravity IDE"));
    }

    #[cfg(target_os = "linux")]
    if let Some(config_home) = crate::modules::linux_paths::config_home() {
        candidate_dirs.push(config_home.join("Antigravity"));
        candidate_dirs.push(config_home.join("Antigravity IDE"));
    }

    for dir in candidate_dirs {
        let app_storage = dir.join("app_storage.json");
        if app_storage.exists() {
            if let Ok(content) = fs::read_to_string(&app_storage) {
                if let Ok(mut json) = serde_json::from_str::<Value>(&content) {
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(
                            "jetski.onboarding.lastLoginUsername".to_string(),
                            Value::String(account.email.clone()),
                        );
                        let is_gcp = if account.token.is_gcp_tos { "true" } else { "false" };
                        obj.insert(
                            "jetski.onboarding.lastLoginIsGcpTos".to_string(),
                            Value::String(is_gcp.to_string()),
                        );
                        if let Some(ref project_id) = account.token.project_id {
                            if !project_id.trim().is_empty() {
                                obj.insert(
                                    "lastCreatedProjectId".to_string(),
                                    Value::String(project_id.clone()),
                                );
                                obj.insert(
                                    "new-convo-last-selected-project".to_string(),
                                    Value::String(project_id.clone()),
                                );
                            }
                        }
                        if let Ok(updated) = serde_json::to_string_pretty(&json) {
                            let _ = fs::write(&app_storage, updated);
                            logger::log_info(&format!("Updated {:?} for {}", app_storage, account.email));
                        }
                    }
                }
            }
        }

        if let Some(ref profile) = account.device_profile {
            let updater_id = dir.join(".updaterId");
            if updater_id.exists() || dir.exists() {
                let _ = fs::write(&updater_id, &profile.dev_device_id);
                logger::log_info(&format!("Updated {:?} with dev_device_id", updater_id));
            }
        }
    }

    Ok(())
}

/// Rewrite every local install identifier from the account profile.
/// storage.json telemetry is written separately by `write_profile`.
pub fn rewrite_install_identifiers(profile: &DeviceProfile) -> Result<(), String> {
    let install_uuid = profile.mac_machine_id.clone();
    let media_salt = profile.mac_machine_id.replace('-', "").to_uppercase();

    if let Some(home) = dirs::home_dir() {
        let gemini = home.join(".gemini").join("antigravity");
        if gemini.is_dir() {
            let installation_id = gemini.join("installation_id");
            fs::write(&installation_id, &install_uuid)
                .map_err(|e| format!("write installation_id: {e}"))?;
            logger::log_info(&format!("Rewrote {:?}", installation_id));

            let state = gemini.join("antigravity_state.pbtxt");
            if state.is_file() {
                let text = fs::read_to_string(&state).map_err(|e| format!("read state: {e}"))?;
                let mut found = false;
                let mut lines: Vec<String> = text
                    .lines()
                    .map(|line| {
                        if line.trim_start().starts_with("installation_uuid:") {
                            found = true;
                            format!("installation_uuid: \"{install_uuid}\"")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect();
                if !found {
                    lines.push(format!("installation_uuid: \"{install_uuid}\""));
                }
                let mut updated = lines.join("\n");
                if text.ends_with('\n') {
                    updated.push('\n');
                }
                fs::write(&state, updated).map_err(|e| format!("write state: {e}"))?;
                logger::log_info(&format!("Rewrote installation_uuid in {:?}", state));
            }
        }
    }

    let mut app_dirs = Vec::new();
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = std::env::var("APPDATA") {
        app_dirs.push(PathBuf::from(appdata).join("Antigravity"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = dirs::home_dir() {
        app_dirs.push(home.join("Library/Application Support/Antigravity"));
    }
    #[cfg(target_os = "linux")]
    if let Some(config_home) = crate::modules::linux_paths::config_home() {
        app_dirs.push(config_home.join("Antigravity"));
    }
    for dir in app_dirs {
        let preferences = dir.join("Preferences");
        if !preferences.is_file() {
            continue;
        }
        let content = fs::read_to_string(&preferences).map_err(|e| format!("read Preferences: {e}"))?;
        let mut json: Value = serde_json::from_str(&content).map_err(|e| format!("parse Preferences: {e}"))?;
        if let Some(root) = json.as_object_mut() {
            let electron = root.entry("electron").or_insert_with(|| Value::Object(Default::default()));
            if let Some(electron) = electron.as_object_mut() {
                let media = electron.entry("media").or_insert_with(|| Value::Object(Default::default()));
                if let Some(media) = media.as_object_mut() {
                    media.insert("device_id_salt".to_string(), Value::String(media_salt.clone()));
                }
            }
        }
        let updated = serde_json::to_string_pretty(&json).map_err(|e| format!("encode Preferences: {e}"))?;
        fs::write(&preferences, updated).map_err(|e| format!("write Preferences: {e}"))?;
        logger::log_info(&format!("Rewrote device_id_salt in {:?}", preferences));
    }
    Ok(())
}

