use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};
use toml_edit::{value, DocumentMut};
use url::Url;

pub const DEFAULT_PORT: u16 = 17682;
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PROBE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub provider: Option<String>,
    pub upstream_base_url: Option<String>,
    pub local_port: u16,
    pub retry_enabled: bool,
    pub enabled: bool,
    pub max_retries: u8,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self { provider: None, upstream_base_url: None, local_port: DEFAULT_PORT,
            retry_enabled: true, enabled: false, max_retries: 4,
            base_delay_ms: 1600, max_delay_ms: 30_000 }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Configuration {
    pub provider: String,
    pub upstream_base_url: String,
    pub local_port: u16,
    pub max_retries: u8,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub name: String,
    pub base_url: String,
    pub active: bool,
    pub wire_api: String,
}

pub fn codex_config_path() -> Result<PathBuf, String> {
    let base = if let Ok(path) = std::env::var("CODEX_HOME") {
        PathBuf::from(path)
    } else {
        dirs::home_dir().ok_or("Unable to find home directory")?.join(".codex")
    };
    Ok(base.join("config.toml"))
}
pub fn load_settings(data_dir: &Path) -> Settings {
    fs::read_to_string(data_dir.join("settings.json")).ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_default()
}
pub fn save_settings(data_dir: &Path, settings: &Settings) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    fs::create_dir_all(data_dir).map_err(|e| format!("Cannot create settings folder: {e}"))?;
    replace_bytes(&data_dir.join("settings.json"), &data, false)
}
pub fn read_document() -> Result<(PathBuf, DocumentMut), String> {
    let path = codex_config_path()?;
    let source = fs::read_to_string(&path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let doc: DocumentMut = source.parse().map_err(|e| format!("Invalid Codex TOML: {e}"))?;
    Ok((path, doc))
}
pub fn providers() -> Result<Vec<ProviderInfo>, String> {
    let (_, doc) = read_document()?;
    let active = doc.get("model_provider").and_then(|x| x.as_str()).unwrap_or_default();
    let mut result = Vec::new();
    if let Some(table) = doc.get("model_providers").and_then(|x| x.as_table()) {
        for (key, item) in table.iter() {
            if let Some(provider) = item.as_table() {
                let wire_api = provider.get("wire_api").and_then(|x| x.as_str()).unwrap_or("");
                let base_url = provider.get("base_url").and_then(|x| x.as_str());
                if let Some(base_url) = base_url {
                    result.push(ProviderInfo {
                        name: key.to_string(), base_url: base_url.to_owned(),
                        active: key == active, wire_api: wire_api.to_owned(),
                    });
                }
            }
        }
    }
    Ok(result)
}
pub fn validate_upstream(raw: &str, port: u16) -> Result<Url, String> {
    if port < 1024 {
        return Err("Local port must be between 1024 and 65535".into());
    }
    let url = Url::parse(raw).map_err(|e| format!("Invalid upstream Base URL: {e}"))?;
    if !["https", "http"].contains(&url.scheme()) {
        return Err("Only HTTP(S) upstream providers are supported".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Do not include credentials, query parameters, or fragments in the provider URL".into());
    }
    let hostname = url.host_str().unwrap_or("");
    let loopback = ["127.0.0.1", "localhost", "::1", "[::1]"].contains(&hostname);
    if url.scheme() != "https" && !loopback {
        return Err("Remote providers must use HTTPS".into());
    }
    let actual_port = url.port_or_known_default().unwrap_or(0);
    if loopback && actual_port == port {
        return Err("Upstream Base URL cannot point to this recovery listener (proxy loop)".into());
    }
    Ok(url)
}
pub fn localhost_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}
pub fn get_url_from_doc(doc: &DocumentMut, provider: &str) -> Result<String, String> {
    if provider.is_empty() { return Err("Select a custom Codex provider first".into()); }
    let item = doc.get("model_providers").and_then(|i| i.get(provider))
        .ok_or_else(|| format!("Provider {provider} is not configured"))?;
    if item.get("wire_api").and_then(|x| x.as_str()) != Some("responses") {
        return Err(format!("Provider {provider} does not use the Responses API"));
    }
    item.get("base_url").and_then(|x| x.as_str())
        .map(str::to_owned).ok_or("Selected provider has no base_url".into())
}
pub fn read_provider_url(provider: &str) -> Result<String, String> {
    let (_, doc) = read_document()?;
    get_url_from_doc(&doc, provider)
}
pub fn switch_provider(provider: &str, expected: &str, target: &str) -> Result<(), String> {
    let (path, mut doc) = read_document()?;
    let current = get_url_from_doc(&doc, provider)?;
    if current == target { return Ok(()); }
    if current != expected {
        return Err(format!(
            "Codex provider was changed outside Recovery Manager (current: {current}). Refusing to overwrite it."
        ));
    }
    doc["model_providers"][provider]["base_url"] = value(target);
    replace_bytes(&path, doc.to_string().as_bytes(), true)
}
pub fn choose_provider(settings: &mut Settings) -> Result<(), String> {
    let known = providers()?;
    if settings.provider.is_none() {
        if let Some(p) = known.iter().find(|p| p.active && p.wire_api == "responses")
            .or_else(|| known.iter().find(|p| p.wire_api == "responses")) {
            settings.provider = Some(p.name.clone());
            settings.upstream_base_url = Some(p.base_url.clone());
        }
    } else if settings.upstream_base_url.is_none() {
        if let Some(p) = known.iter().find(|p| Some(&p.name) == settings.provider.as_ref()) {
            settings.upstream_base_url = Some(p.base_url.clone());
        }
    }
    Ok(())
}
pub fn validate_configuration(cfg: &Configuration, current: &Settings) -> Result<(), String> {
    if cfg.provider.trim().is_empty() { return Err("Choose a provider".into()); }
    validate_upstream(&cfg.upstream_base_url, cfg.local_port)?;
    if cfg.max_retries > 10 { return Err("Max retries must be 0–10".into()); }
    if !(200..=60_000).contains(&cfg.base_delay_ms) { return Err("Initial backoff must be 200–60000 ms".into()); }
    if !(1000..=180_000).contains(&cfg.max_delay_ms) || cfg.max_delay_ms < cfg.base_delay_ms {
        return Err("Maximum backoff must be >= initial backoff, and <= 180000 ms".into());
    }
    if current.enabled { return Err("Disable recovery before changing provider settings".into()); }
    let (_, doc) = read_document()?;
    let configured = get_url_from_doc(&doc, &cfg.provider)?;
    let old = current.upstream_base_url.as_deref();
    if configured != cfg.upstream_base_url && old != Some(configured.as_str()) {
        return Err("Provider was externally modified. Refresh the connection settings before saving.".into());
    }
    Ok(())
}
pub fn apply_configuration(cfg: &Configuration, settings: &mut Settings) -> Result<(), String> {
    validate_configuration(cfg, settings)?;
    let current_url = read_provider_url(&cfg.provider)?;
    if current_url != cfg.upstream_base_url {
        switch_provider(&cfg.provider, &current_url, &cfg.upstream_base_url)?;
    }
    settings.provider = Some(cfg.provider.clone());
    settings.upstream_base_url = Some(cfg.upstream_base_url.clone());
    settings.local_port = cfg.local_port;
    settings.max_retries = cfg.max_retries;
    settings.base_delay_ms = cfg.base_delay_ms;
    settings.max_delay_ms = cfg.max_delay_ms;
    Ok(())
}
fn replace_bytes(dest: &Path, bytes: &[u8], backup: bool) -> Result<(), String> {
    let parent = dest.parent().ok_or("File has no parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temp = parent.join(format!(".codex-recovery-{}.tmp", std::process::id()));
    fs::write(&temp, bytes).map_err(|e| format!("Writing temporary file failed: {e}"))?;
    let result = (|| {
        if dest.exists() {
            if backup {
                let backup_path = dest.with_extension("toml.before-codex-recovery.bak");
                if !backup_path.exists() {
                    fs::copy(dest, &backup_path).map_err(|e| format!("Cannot back up Codex config: {e}"))?;
                }
            }
            #[cfg(windows)]
            {
                use std::{ffi::OsStr, os::windows::ffi::OsStrExt};
                use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
                fn to_wide(s: &OsStr) -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() }
                let a = to_wide(dest.as_os_str());
                let b = to_wide(temp.as_os_str());
                let ok = unsafe { ReplaceFileW(a.as_ptr(), b.as_ptr(), std::ptr::null(), 0, std::ptr::null_mut(), std::ptr::null_mut()) };
                if ok == 0 { return Err(format!("Could not atomically replace Codex config: {}", std::io::Error::last_os_error())); }
            }
            #[cfg(not(windows))]
            fs::rename(&temp, dest).map_err(|e| format!("Atomic replacement failed: {e}"))?;
        } else {
            fs::rename(&temp, dest).map_err(|e| format!("Creating settings file failed: {e}"))?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base_urls_accept_custom_https_paths() {
        assert!(validate_upstream("https://gateway.example/api/compatible/v1", 17682).is_ok());
        assert!(validate_upstream("http://localhost:3333/v1", 17682).is_ok());
        assert!(validate_upstream("http://public.example/v1", 17682).is_err());
        assert!(validate_upstream("http://127.0.0.1:17682/v1", 17682).is_err());
        assert!(validate_upstream("https://foo:bar@public.example/v1", 17682).is_err());
    }
    #[test]
    fn localhost_scheme_is_stable() {
        assert_eq!(localhost_url(17682), "http://127.0.0.1:17682/v1");
    }
    #[test]
    fn provider_rewrite_keeps_unrelated_settings_and_restores_direct_url() {
        let original=std::env::var_os("CODEX_HOME");
        let temp=std::env::temp_dir().join(format!("codex-recovery-test-{}",std::process::id()));
        let _=std::fs::create_dir_all(&temp);
        let file=temp.join("config.toml");
        let fixture=r#"
model_provider = "example"
[model_providers.example]
name = "Example"
wire_api = "responses"
base_url = "https://gateway.example/custom/api/v1" # keep this comment
request_max_retries = 3
[model_providers.other]
name = "Other"
wire_api = "responses"
base_url = "https://other.example/v1"
"#;
        std::fs::write(&file,fixture).unwrap();
        std::env::set_var("CODEX_HOME",&temp);
        let provider=providers().unwrap();
        assert_eq!(provider.len(),2);
        assert!(provider.iter().any(|v|v.name=="example"&&v.active));
        let local=localhost_url(17682);
        switch_provider("example","https://gateway.example/custom/api/v1",&local).unwrap();
        assert_eq!(read_provider_url("example").unwrap(),local);
        let content=std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("request_max_retries = 3"));
        assert!(content.contains("https://other.example/v1"));
        switch_provider("example",&local,"https://gateway.example/custom/api/v1").unwrap();
        assert_eq!(read_provider_url("example").unwrap(),"https://gateway.example/custom/api/v1");
        if let Some(old)=original {std::env::set_var("CODEX_HOME",old);}else{std::env::remove_var("CODEX_HOME");}
        let _=std::fs::remove_dir_all(temp);
    }
}
