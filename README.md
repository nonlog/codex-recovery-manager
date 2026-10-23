# Codex Recovery Manager

[![Windows native build](https://github.com/nonlog/codex-recovery-manager/actions/workflows/windows.yml/badge.svg)](https://github.com/nonlog/codex-recovery-manager/actions/workflows/windows.yml)

Native Windows desktop application built with Tauri 2 / Rust + React; not a PowerShell manager or Node.js service. System tray, visual controls, original provider preservation and a Rust Responses retry proxy in one application. This is an independent open-source project, not affiliated with OpenAI.

## Features

- Native dark/light desktop GUI and Windows tray; closing the window does not end the process.
- In-process Rust model proxy listening only on 127.0.0.1.
- Bounded retry for recognized capacity errors and early SSE stream interruption before meaningful model output.
- Pause retries without disconnecting the current proxy route; resume without restarting.
- Disable restores the original upstream URL *before* stopping the local server.
- Detects Responses-compatible custom Codex providers; configurable upstream Base URL, port and retry policy.
- Start with Windows, single-instance operation, no Node.js, PowerShell or separate service necessary.
- Windows NSIS installer and portable EXE built by GitHub Actions.

## Installation

Download the latest validated Windows installer from [Actions](https://github.com/nonlog/codex-recovery-manager/actions/workflows/windows.yml) or [Releases](https://github.com/nonlog/codex-recovery-manager/releases).

Open the app; choose your configured Codex Responses provider. Confirm its existing Base URL on the Configuration tab. Click Enable. The local listener must start and pass a health check before Codex config is redirected. Restart any already-running Codex agent to reload its Provider URL.

Custom Codex provider example:

~~~toml
model_provider = "example"
[model_providers.example]
name = "Example API"
base_url = "https://gateway.example/v1"
wire_api = "responses"
requires_openai_auth = true
~~~

Providers must have an explicit custom Base URL and wire_api = "responses". The built-in ChatGPT OAuth provider cannot currently be transparently proxied.

## Pause, Disable and Quit

- **Pause** switches off retry policy but keeps forwarding requests. This is the correct choice if you wish to keep existing active agents connected.
- **Disable** first changes Codex's original URL back, then shuts down localhost. Already-running Codex clients may cache the old proxy URL and require restarting.
- **X** hides the UI to tray, leaving the proxy alive.
- **Quit** via tray attempts a safe disable before exit; quitting is blocked while model requests are active.

## Migrating from the old Node.js / PowerShell recovery manager

If Codex currently points to a localhost port owned by a previous script-based recovery tool, disable the **old** recovery manager and restore the original gateway Base URL before enabling this native app. Fully exit its process and remove its legacy startup task. The native app will reject a port conflict rather than redirect Codex to an unverified listener.

Do not copy anyone's local recovery settings or authentication files into the shared distribution.

## Security and caveats

No provider URL or credential belonging to the developer is shipped. Personal config stays in the user's application config directory. Authentication headers pass through without being stored; model prompts and response bodies are not logged. Remote upstreams must use HTTPS, and URL loops are rejected. The app only modifies the selected provider's base_url in ~/.codex/config.toml; unknown external edits are not overwritten.

After unexpected force termination, existing clients can still have a stale localhost address cached. On next start, the application attempts to restore the service or original URL. This is a request-level recovery tool, *not* a session-level IPC watchdog; it will not retry already delivered tool calls.

## Development

Use GitHub Actions for Rust tests, native Windows compilation, installation bundles and formal verification. See [.github/workflows/windows.yml](.github/workflows/windows.yml).

MIT licensed. 
