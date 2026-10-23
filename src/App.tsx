import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion } from "motion/react";
import { create } from "zustand";
import * as Switch from "@radix-ui/react-switch";
import {
  Activity, ArrowUpRight, Check, CircleAlert, CircleCheck, Clock3,
  ExternalLink, Github, Layers3, LoaderCircle, LockKeyhole,
  Monitor, Pause, Play, PlugZap, RefreshCw, RotateCcw, Save, Settings2,
  ShieldCheck, SlidersHorizontal, Wifi, WifiOff, Zap
} from "lucide-react";

type Mode = "enabled" | "paused" | "direct" | "degraded";
type Provider = { name: string; baseUrl: string; active: boolean; wireApi: string };
type Snapshot = {
  mode: Mode;
  provider: string | null;
  upstreamBaseUrl: string | null;
  localPort: number;
  retryEnabled: boolean;
  maxRetries: number;
  baseDelayMs: number;
  maxDelayMs: number;
  autoLaunch: boolean;
  serviceHealthy: boolean;
  lastError: string | null;
  metrics: { requests: number; retries: number; recovered: number; exhausted: number; capacity: number; earlyStreams: number; active: number };
  providers: Provider[];
};
type Configuration = { provider: string; upstreamBaseUrl: string; localPort: number; maxRetries: number; baseDelayMs: number; maxDelayMs: number };
type SettingsStore = {
  snapshot: Snapshot | null;
  busy: string | null;
  error: string | null;
  setSnapshot: (value: Snapshot) => void;
  setBusy: (value: string | null) => void;
  setError: (value: string | null) => void;
};
const useApp = create<SettingsStore>(set => ({
  snapshot: null, busy: null, error: null,
  setSnapshot: snapshot => set({snapshot}),
  setBusy: busy => set({busy}),
  setError: error => set({error})
}));

const modeLabels: Record<Mode, { title: string; description: string; tone: string }> = {
  enabled: { title: "Auto recovery active", description: "Codex → local recovery → model provider", tone: "green" },
  paused: { title: "Retries paused", description: "Requests still flow normally. No interruption.", tone: "amber" },
  direct: { title: "Direct connection", description: "Codex connects directly to its existing provider.", tone: "neutral" },
  degraded: { title: "Connection needs attention", description: "The local proxy is not healthy. Check status below.", tone: "red" }
};

function App() {
  const {snapshot, busy, error, setSnapshot, setBusy, setError} = useApp();
  const [draft, setDraft] = useState<Configuration>({
    provider: "", upstreamBaseUrl: "", localPort: 17682, maxRetries: 4, baseDelayMs: 1600, maxDelayMs: 30000
  });
  const [initialised, setInitialised] = useState(false);
  const [tab, setTab] = useState<"overview" | "settings">("overview");
  const [dark, setDark] = useState(() => localStorage.getItem("theme") !== "light");

  useEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    localStorage.setItem("theme", dark ? "dark" : "light");
  }, [dark]);

  async function refresh(preserve = true) {
    try {
      const result = await invoke<Snapshot>("get_status");
      setSnapshot(result);
      if (!preserve || !initialised) {
        const matched = result.providers.find(p => p.name === result.provider) ?? result.providers.find(p => p.active) ?? result.providers[0];
        setDraft({
          provider: matched?.name ?? "",
          upstreamBaseUrl: result.upstreamBaseUrl ?? matched?.baseUrl ?? "",
          localPort: result.localPort,
          maxRetries: result.maxRetries,
          baseDelayMs: result.baseDelayMs,
          maxDelayMs: result.maxDelayMs
        });
        setInitialised(true);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    void refresh(false);
    const handle = setInterval(() => { if (!useApp.getState().busy) void refresh(); }, 3500);
    return () => clearInterval(handle);
  }, []);

  async function action(name: string, command: string, args: Record<string, unknown> = {}) {
    if (busy) return;
    setBusy(name);
    setError(null);
    try {
      await invoke(command, args);
      await refresh(false);
    } catch (e) {
      setError(String(e));
      await refresh();
    } finally {
      setBusy(null);
    }
  }

  const mode = snapshot?.mode ?? "direct";
  const state = modeLabels[mode];
  const editable = mode === "direct" && !busy;
  const currentProvider = snapshot?.providers.find(p => p.name === draft.provider);
  const isDirty = useMemo(() => {
    if (!snapshot) return false;
    return draft.provider !== (snapshot.provider ?? snapshot.providers.find(p => p.active)?.name ?? "") ||
      draft.upstreamBaseUrl !== (snapshot.upstreamBaseUrl ?? snapshot.providers.find(p => p.name === draft.provider)?.baseUrl ?? "") ||
      draft.localPort !== snapshot.localPort || draft.maxRetries !== snapshot.maxRetries ||
      draft.baseDelayMs !== snapshot.baseDelayMs || draft.maxDelayMs !== snapshot.maxDelayMs;
  }, [draft, snapshot]);

  return (
    <main className="shell">
      <aside className="sidebar">
        <div className="brand"><div className="brand-icon"><RotateCcw size={19} strokeWidth={2.35}/></div><div><strong>Codex Recovery</strong><span>MANAGER</span></div></div>
        <div className="side-label">WORKSPACE</div>
        <nav className="nav">
          <button className={tab === "overview" ? "active" : ""} onClick={() => setTab("overview")}><Activity size={17}/> Overview</button>
          <button className={tab === "settings" ? "active" : ""} onClick={() => setTab("settings")}><SlidersHorizontal size={17}/> Configuration</button>
        </nav>
        <div className="sidebar-bottom">
          <div className="side-note"><ShieldCheck size={15}/><span>Credentials never stored</span></div>
          <a href="https://github.com/nonlog/codex-recovery-manager" target="_blank" rel="noreferrer"><Github size={16}/> GitHub Repository <ArrowUpRight size={13}/></a>
          <div className="version">OPEN SOURCE <span>v0.1.0</span></div>
        </div>
      </aside>
      <section className="content">
        <header className="topbar"><div className="breadcrumb">Codex Recovery <span>/</span> {tab === "overview" ? "Overview" : "Configuration"}</div>
          <div className="top-actions">
            <button className="icon-button" title="Toggle theme" onClick={() => setDark(!dark)}><Monitor size={16}/></button>
            <button className="icon-button" title="Refresh" onClick={() => void refresh()}><RefreshCw size={16}/></button>
          </div>
        </header>

        {error && <div role="alert" className="error-banner"><CircleAlert size={17}/><div><strong>Operation did not complete</strong><p>{error}</p></div><button onClick={() => setError(null)}>Dismiss</button></div>}

        {tab === "overview" ? (
          <motion.div initial={{opacity:0,y:8}} animate={{opacity:1,y:0}} transition={{duration:.22}} className="main-panel">
            <div className="heading"><div><div className="eyebrow"><Zap size={14}/> RELIABILITY LAYER</div><h1>Model recovery, without the terminal.</h1><p>Automatic capacity retries for any configured Responses-compatible Codex provider.</p></div></div>
            <div className={"hero-status "+state.tone}>
              <div className="status-icon">{mode === "enabled" ? <CircleCheck size={21}/> : mode === "degraded" ? <WifiOff size={21}/> : <Wifi size={21}/>}</div>
              <div className="status-copy"><div className="status-title">{snapshot ? state.title : "Connecting…"}</div><div className="status-description">{state.description}</div></div>
              <span className="small-pill"><span className="live-dot"/> {snapshot?.serviceHealthy ? "PROXY ONLINE" : "DIRECT / OFFLINE"}</span>
            </div>
            <div className="stats">
              {([
                {label:"Model requests",value:snapshot?.metrics.requests,icon:Layers3},
                {label:"Automatic retries",value:snapshot?.metrics.retries,icon:RefreshCw},
                {label:"Recovered",value:snapshot?.metrics.recovered,icon:Check},
                {label:"Active requests",value:snapshot?.metrics.active,icon:Activity}
              ]).map(c => <div className="stat" key={c.label}><div className="stat-top"><span>{c.label}</span><c.icon size={16}/></div><strong>{c.value ?? "—"}</strong></div>)}
            </div>
            <div className="panel">
              <div className="panel-heading"><div><strong>Recovery controls</strong><p>Pause retries without disconnecting your active agent.</p></div><div className="meta-label"><LockKeyhole size={13}/> Local only</div></div>
              <div className="controls">
                {mode === "direct" ? <button disabled={Boolean(busy)} className="primary control-button" onClick={() => action("enable", "enable_recovery")}><Play size={17}/> Enable recovery</button> :
                 mode === "paused" ? <button disabled={Boolean(busy)} className="primary control-button" onClick={() => action("resume", "resume_recovery")}><Play size={17}/> Resume retries</button> :
                 <button disabled={Boolean(busy) || mode === "degraded"} className="primary control-button" onClick={() => action("pause", "pause_recovery")}><Pause size={17}/> Pause retries</button>}
                <button disabled={Boolean(busy) || mode === "direct"} className="secondary control-button" onClick={() => action("disable", "disable_recovery")}><PlugZap size={17}/> Disable & restore direct</button>
                {busy && <LoaderCircle size={17} className="spin"/>}
              </div>
              <div className="helper"><CircleAlert size={15}/><span>Closing the window hides it to the tray. Use <b>Pause</b> instead of quitting to keep existing requests online.</span></div>
            </div>
            <div className="footer-line"><span>Current provider: <b>{snapshot?.provider ?? "Not configured"}</b></span><span>Local port: <b>{snapshot?.localPort ?? "—"}</b></span></div>
          </motion.div>
        ) : (
          <motion.div initial={{opacity:0,y:8}} animate={{opacity:1,y:0}} transition={{duration:.22}} className="main-panel">
            <div className="heading"><div><div className="eyebrow"><Settings2 size={14}/> PROVIDER SETTINGS</div><h1>Connection settings</h1><p>Your upstream is discovered from Codex, not hardcoded into this app.</p></div></div>
            <div className="panel settings-panel">
              <div className="panel-heading"><div><strong>Model provider</strong><p>Select a custom Responses provider from your Codex config.</p></div></div>
              <div className="form-grid">
                <label className="full"><span>Codex provider</span>
                  <select disabled={!editable} value={draft.provider} onChange={e => {
                    const next = snapshot?.providers.find(p => p.name === e.target.value);
                    setDraft(v => ({...v,provider:e.target.value,upstreamBaseUrl:next?.baseUrl ?? ""}));
                  }}>
                    {snapshot?.providers.length ? snapshot.providers.map(p => <option key={p.name} value={p.name}>{p.name}{p.active ? " (active)" : ""}</option>) : <option value="">No custom Responses provider detected</option>}
                  </select>
                </label>
                <label className="full"><span>Upstream Base URL</span><input disabled={!editable} spellCheck={false} placeholder="https://gateway.example/v1" value={draft.upstreamBaseUrl}
                    onChange={e => setDraft(v => ({...v,upstreamBaseUrl:e.target.value}))}/></label>
                <label><span>Local listener port</span><input disabled={!editable} type="number" min={1024} max={65535} value={draft.localPort} onChange={e => setDraft(v=>({...v,localPort:Number(e.target.value)}))}/></label>
                <label><span>Retries per request</span><input disabled={!editable} type="number" min={0} max={10} value={draft.maxRetries} onChange={e=>setDraft(v=>({...v,maxRetries:Number(e.target.value)}))}/></label>
                <label><span>Initial backoff (ms)</span><input disabled={!editable} type="number" min={200} max={60000} value={draft.baseDelayMs} onChange={e=>setDraft(v=>({...v,baseDelayMs:Number(e.target.value)}))}/></label>
                <label><span>Maximum backoff (ms)</span><input disabled={!editable} type="number" min={1000} max={180000} value={draft.maxDelayMs} onChange={e=>setDraft(v=>({...v,maxDelayMs:Number(e.target.value)}))}/></label>
              </div>
              {currentProvider && currentProvider.wireApi !== "responses" && <div className="warning"><CircleAlert size={16}/> This provider uses {currentProvider.wireApi}; only Responses providers are supported.</div>}
              <div className="settings-actions"><button className="secondary" onClick={() => void refresh(false)} disabled={Boolean(busy)}><RotateCcw size={15}/> Reset</button><button className="primary" disabled={!editable || !draft.provider || !isDirty || Boolean(busy)} onClick={() => action("save","save_configuration",{configuration:draft})}><Save size={15}/> Save configuration</button></div>
            </div>
            <div className="panel tiny-panel">
              <div className="autostart-info"><div><strong>Start with Windows</strong><p>Launch quietly into the tray and restore a previously enabled proxy.</p></div>
                <Switch.Root className="switch" checked={snapshot?.autoLaunch ?? false} disabled={Boolean(busy)} onCheckedChange={checked => action("autostart","set_autostart",{enabled:checked})}><Switch.Thumb className="thumb"/></Switch.Root>
              </div>
            </div>
            <div className="helper outside"><ShieldCheck size={15}/> Existing Codex processes may cache the previous URL. Restart that agent after switching between direct and proxy.</div>
          </motion.div>
        )}
        <div className="copyright"><span><CircleCheck size={13}/> Requests and credentials are not logged</span><span><Clock3 size={13}/> Windows desktop</span><a href="https://github.com/nonlog/codex-recovery-manager/issues" target="_blank" rel="noreferrer"><ExternalLink size={13}/> Feedback</a></div>
      </section>
    </main>
  );
}
export default App;
