mod config;
mod proxy;

use config::{Configuration, ProviderInfo, Settings};
use proxy::{Counters, Metrics, ProxyContext};
use serde::Serialize;
use std::{path::PathBuf, sync::{Arc, RwLock}, time::Duration};
use tauri::{menu::{Menu, MenuItem}, tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent}, AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{ManagerExt as AutostartManagerExt, MacosLauncher};
use tokio::{sync::{Mutex as AsyncMutex, oneshot}, task::JoinHandle};

pub struct AppRuntime {
    data_dir: PathBuf,
    settings: Arc<RwLock<Settings>>,
    metrics: Arc<Metrics>,
    server: AsyncMutex<Option<ServerHandle>>,
    operation: AsyncMutex<()>,
    last_error: RwLock<Option<String>>,
}
struct ServerHandle { shutdown: Option<oneshot::Sender<()>>, task: JoinHandle<()> }

impl AppRuntime {
    fn new(data_dir:PathBuf) -> Self {
        let mut settings=config::load_settings(&data_dir);
        if let Err(err)=config::choose_provider(&mut settings) {
            eprintln!("Provider discovery skipped: {err}");
        }
        Self {
            data_dir, settings:Arc::new(RwLock::new(settings)),
            metrics:Arc::new(Metrics::default()),server:AsyncMutex::new(None),
            operation:AsyncMutex::new(()),last_error:RwLock::new(None),
        }
    }
    fn settings(&self) -> Result<Settings,String> {
        self.settings.read().map(|x| x.clone()).map_err(|_|"Recovery state locked".into())
    }
    fn update(&self, new_settings:Settings) -> Result<(),String> {
        config::save_settings(&self.data_dir,&new_settings)?;
        *self.settings.write().map_err(|_|"Recovery state locked".to_string())?=new_settings;
        Ok(())
    }
    fn remember_error(&self, error:String) {
        if let Ok(mut field)=self.last_error.write() { *field=Some(error); }
    }
    async fn start_server(&self, settings:&Settings) -> Result<(),String> {
        let mut server=self.server.lock().await;
        if let Some(handle)=server.as_ref() {
            if !handle.task.is_finished() { return Ok(()); }
        }
        // Bind before rewriting config.toml. No listener means no rerouting.
        let listener=tokio::net::TcpListener::bind(("127.0.0.1",settings.local_port)).await
            .map_err(|e|format!("Port {} unavailable: {e}",settings.local_port))?;
        let ctx=ProxyContext::new(self.settings.clone(),self.metrics.clone())?;
        let (tx,rx)=oneshot::channel();
        let task=tokio::spawn(async move {
            if let Err(error)=proxy::run_server(listener,ctx,rx).await {
                eprintln!("Proxy stopped: {error}");
            }
        });
        *server=Some(ServerHandle {shutdown:Some(tx), task});
        drop(server);
        let url=format!("http://127.0.0.1:{}/__health",settings.local_port);
        let client=reqwest::Client::builder().timeout(Duration::from_secs(2)).build()
            .map_err(|e|format!("Health check initialization: {e}"))?;
        for _ in 0..12 {
            if let Ok(res)=client.get(&url).send().await {
                if res.status().is_success() {return Ok(());}
            }
            tokio::time::sleep(Duration::from_millis(110)).await;
        }
        self.stop_server().await;
        Err("Local recovery service failed its health check. Codex was not redirected.".into())
    }
    async fn stop_server(&self) {
        let mut current=self.server.lock().await;
        if let Some(mut server)=current.take() {
            if let Some(tx)=server.shutdown.take() { let _=tx.send(()); }
            if tokio::time::timeout(Duration::from_secs(7),&mut server.task).await.is_err() {
                server.task.abort();
            }
        }
    }
    async fn running(&self) -> bool {
        self.server.lock().await.as_ref().is_some_and(|x|!x.task.is_finished())
    }
}
#[derive(Serialize)]
#[serde(rename_all="camelCase")]
struct AppStatus {
    mode: &'static str,
    provider: Option<String>,
    upstream_base_url: Option<String>,
    local_port: u16,
    retry_enabled: bool,
    max_retries:u8,
    base_delay_ms:u64,
    max_delay_ms:u64,
    auto_launch:bool,
    service_healthy:bool,
    last_error:Option<String>,
    metrics:Counters,
    providers:Vec<ProviderInfo>,
}

async fn enable_core(rt:&Arc<AppRuntime>) -> Result<(),String> {
    let _guard=rt.operation.lock().await;
    let mut cfg=rt.settings()?;
    if cfg.enabled && rt.running().await {return Ok(());}
    let provider=cfg.provider.clone().ok_or("No Responses-compatible provider selected")?;
    if !config::providers()?.iter().any(|p| p.active && p.name==provider && p.wire_api=="responses") {
        return Err("The selected provider is not active in Codex. Select the active Responses provider or update model_provider in Codex first.".into());
    }
    let upstream=cfg.upstream_base_url.clone().ok_or("No upstream provider URL selected")?;
    config::validate_upstream(&upstream,cfg.local_port)?;
    let current=config::read_provider_url(&provider)?;
    let local=config::localhost_url(cfg.local_port);
    if current!=upstream && current!=local {
        return Err(format!("Provider {provider} was changed externally. Refresh configuration before enabling."));
    }
    rt.start_server(&cfg).await?;
    if current!=local {
        if let Err(error)=config::switch_provider(&provider,&upstream,&local) {
            rt.stop_server().await;
            return Err(error);
        }
    }
    cfg.enabled=true;
    cfg.retry_enabled=true;
    if let Err(error)=rt.update(cfg) {
        let _=config::switch_provider(&provider,&local,&upstream);
        rt.stop_server().await;
        return Err(error);
    }
    Ok(())
}

async fn disable_core(rt:&Arc<AppRuntime>) -> Result<(),String> {
    let _guard=rt.operation.lock().await;
    let mut cfg=rt.settings()?;
    if rt.metrics.active.load(std::sync::atomic::Ordering::Relaxed)>0 {
        return Err("An upstream request is in progress. Pause first or wait for it to complete before disabling.".into());
    }
    if let (Some(provider),Some(upstream))=(&cfg.provider,&cfg.upstream_base_url) {
        let local=config::localhost_url(cfg.local_port);
        let current=config::read_provider_url(provider)?;
        if current==local {
            // Restore direct routing BEFORE stopping the bound listener.
            config::switch_provider(provider,&local,upstream)?;
        } else if current!=*upstream {
            // An external agent changed the provider. Leave its configuration intact.
            rt.remember_error(format!("Provider {provider} was changed outside Recovery Manager. Original route was left untouched."));
        }
    }
    cfg.enabled=false;
    cfg.retry_enabled=false;
    rt.update(cfg)?;
    rt.stop_server().await;
    Ok(())
}

async fn pause_core(rt:&Arc<AppRuntime>,enabled:bool) -> Result<(),String> {
    let _guard=rt.operation.lock().await;
    let mut settings=rt.settings()?;
    if !settings.enabled || !rt.running().await {
        return Err("Local recovery service is not running. Enable it before pausing or resuming.".into());
    }
    settings.retry_enabled=enabled;
    rt.update(settings)
}

#[tauri::command]
async fn enable_recovery(state:State<'_,Arc<AppRuntime>>, app:AppHandle) -> Result<(),String> {
    // A persistent localhost Provider must never be enabled without next-login recovery.
    app.autolaunch().enable().map_err(|e|format!("Windows autostart could not be enabled; Codex was not redirected: {e}"))?;
    enable_core(state.inner()).await
}
#[tauri::command]
async fn disable_recovery(state:State<'_,Arc<AppRuntime>>) -> Result<(),String> {
    disable_core(state.inner()).await
}
#[tauri::command]
async fn pause_recovery(state:State<'_,Arc<AppRuntime>>) -> Result<(),String> {
    pause_core(state.inner(),false).await
}
#[tauri::command]
async fn resume_recovery(state:State<'_,Arc<AppRuntime>>) -> Result<(),String> {
    pause_core(state.inner(),true).await
}
#[tauri::command]
async fn save_configuration(
    configuration:Configuration, state:State<'_,Arc<AppRuntime>>
) -> Result<(),String> {
    let _guard=state.operation.lock().await;
    let mut settings=state.settings()?;
    config::apply_configuration(&configuration,&mut settings)?;
    state.update(settings)
}
#[tauri::command]
async fn get_status(
    state:State<'_,Arc<AppRuntime>>, app:AppHandle
) -> Result<AppStatus,String> {
    let cfg=state.settings()?;
    let running=state.running().await;
    let local=config::localhost_url(cfg.local_port);
    let url=cfg.provider.as_deref().and_then(|name|config::read_provider_url(name).ok());
    let mode=if cfg.enabled {
        if !running || url.as_deref()!=Some(local.as_str()) {"degraded"}
        else if cfg.retry_enabled {"enabled"}else{"paused"}
    }else{"direct"};
    let providers=config::providers().unwrap_or_default();
    Ok(AppStatus {
        mode,provider:cfg.provider,
        upstream_base_url:cfg.upstream_base_url,
        local_port:cfg.local_port,retry_enabled:cfg.retry_enabled,
        max_retries:cfg.max_retries,base_delay_ms:cfg.base_delay_ms,max_delay_ms:cfg.max_delay_ms,
        auto_launch:app.autolaunch().is_enabled().unwrap_or(false),
        service_healthy:running,
        last_error:state.last_error.read().ok().and_then(|v|v.clone()),
        metrics:state.metrics.snapshot(),providers
    })
}
#[tauri::command]
fn set_autostart(app:AppHandle,state:State<'_,Arc<AppRuntime>>,enabled:bool) -> Result<(),String> {
    if !enabled && state.settings()?.enabled {
        return Err("Disable recovery and restore the upstream before disabling Windows autostart. Otherwise a reboot could leave Codex on a dead localhost endpoint.".into());
    }
    let launcher=app.autolaunch();
    if enabled {launcher.enable().map_err(|e|e.to_string())}
    else {launcher.disable().map_err(|e|e.to_string())}
}

fn show_main(app:&AppHandle) {
    if let Some(window)=app.get_webview_window("main") {
        let _=window.show();
        let _=window.unminimize();
        let _=window.set_focus();
    }
}
fn setup_tray(app:&AppHandle) -> tauri::Result<()> {
    let show=MenuItem::with_id(app,"show","Open recovery manager",true,None::<&str>)?;
    let quit=MenuItem::with_id(app,"quit","Quit (restore direct provider)",true,None::<&str>)?;
    let menu=Menu::with_items(app,&[&show,&quit])?;
    let mut builder=TrayIconBuilder::new().menu(&menu).show_menu_on_left_click(false);
    if let Some(icon)=app.default_window_icon() { builder=builder.icon(icon.clone()); }
    builder
        .on_menu_event(|app,event| {
            match event.id().as_ref() {
                "show"=>show_main(app),
                "quit"=>{
                    let app=app.clone();
                    tauri::async_runtime::spawn(async move {
                        let runtime=app.state::<Arc<AppRuntime>>().inner().clone();
                        if let Err(error)=disable_core(&runtime).await {
                            runtime.remember_error(format!("Quit was prevented: {error}"));
                            show_main(&app);
                        }else{
                            app.exit(0);
                        }
                    });
                }
                _=>{}
            }
        })
        .on_tray_icon_event(|tray,event| {
            if let TrayIconEvent::Click { button:MouseButton::Left,button_state:MouseButtonState::Up,.. }=event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app,_,_|show_main(app)))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,Some(vec!["--background"])
        ))
        .setup(|app| {
            let dir=app.path().app_config_dir()?;
            let runtime=Arc::new(AppRuntime::new(dir));
            app.manage(runtime.clone());
            setup_tray(app.handle())?;
            if std::env::args().any(|x|x=="--background") {
                if let Some(win)=app.get_webview_window("main") {let _=win.hide();}
            }
            tauri::async_runtime::spawn(async move {
                let saved=match runtime.settings(){Ok(v)=>v,Err(_)=>return};
                if let (Some(provider),Some(upstream))=(&saved.provider,&saved.upstream_base_url) {
                    let local=config::localhost_url(saved.local_port);
                    if saved.enabled {
                        if let Err(error)=enable_core(&runtime).await {
                            runtime.remember_error(format!("Startup recovery failed: {error}"));
                            // If we cannot restart the proxy, restore a direct route for next clients.
                            if config::read_provider_url(provider).ok().as_deref()==Some(local.as_str()) {
                                let _=config::switch_provider(provider,&local,upstream);
                            }
                            if let Ok(mut s)=runtime.settings(){s.enabled=false;s.retry_enabled=false;let _=runtime.update(s);}
                            runtime.stop_server().await;
                        }
                    }else if config::read_provider_url(provider).ok().as_deref()==Some(local.as_str()) {
                        let _=config::switch_provider(provider,&local,upstream);
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window,event| {
            if let WindowEvent::CloseRequested {api,..}=event {
                api.prevent_close();
                let _=window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,save_configuration,enable_recovery,disable_recovery,
            pause_recovery,resume_recovery,set_autostart
        ])
        .run(tauri::generate_context!())
        .expect("Unable to start Codex Recovery Manager");
}
