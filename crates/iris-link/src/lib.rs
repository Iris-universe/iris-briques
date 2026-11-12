//! iris-link côté app, en Rust/Tauri 2 : rend une app pilotable par l'assistant (Orbiz).
//!
//! Protocole (identique à la version Node) : named pipe `\\.\pipe\iris-link-<id>`, un message JSON
//! par ligne (`ping`, `manifest`, `call`), manifeste écrit dans `%APPDATA%\IrisLink\apps\<id>.json`.
//!
//! - Les outils déclarés par l'app sont exécutés dans l'interface : évènement `agent-call`
//!   `{ id, kind: "page", tool, args }` -> `window.irisAgent[tool](args)` -> commande `agent_reply`.
//! - Les outils génériques (`get_screen`, `click`, `type_text`…) arrivent avec `kind: "ui"`.
//! - `screenshot` et `show_window` sont gérés ici.
//!
//! ```ignore
//! tauri::Builder::default()
//!     .invoke_handler(tauri::generate_handler![iris_link::commands::agent_reply, iris_link::commands::assistant_ask])
//!     .setup(|app| {
//!         iris_link::start(app.handle(), iris_link::AppInfo { id: "monapp", name: "Mon App", description: "…", instructions: "…", tools: tools() });
//!         Ok(())
//!     })
//! ```

mod screenshot;
pub use screenshot::capture as screenshot;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::oneshot;

/// Description de l'app pour l'assistant.
pub struct AppInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub instructions: &'static str,
    /// Tableau d'outils `{ name, description, parameters }` (voir [`obj`]).
    pub tools: Value,
}

/// Schéma JSON d'un objet : `obj(json!({ "id": { "type": "string" } }), json!(["id"]))`.
pub fn obj(properties: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// Outils génériques exécutés par le script injecté dans la page.
pub const UI_TOOLS: [&str; 7] = ["get_screen", "list_elements", "click", "type_text", "press_key", "scroll", "execute_javascript"];
const NATIVE_TOOLS: [&str; 2] = ["screenshot", "show_window"];

pub(crate) struct Link {
    pub(crate) info: AppInfo,
    page_tools: Vec<String>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

fn root() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("IrisLink")
}

fn manifest(info: &AppInfo) -> Value {
    let exe = std::env::current_exe().unwrap_or_default();
    let cwd = exe.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    json!({
        "id": info.id, "name": info.name, "description": info.description, "instructions": info.instructions,
        "pid": std::process::id(),
        "launch": { "command": exe.to_string_lossy(), "args": [], "cwd": cwd },
        "tools": info.tools,
        "updated": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    })
}

/// Réponse de l'interface à un `agent-call` (utilisée par la commande [`agent_reply`]).
pub fn reply(app: &AppHandle, id: u64, result: Value) {
    if let Some(l) = app.try_state::<Link>() {
        if let Some(tx) = l.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(result);
        }
    }
}

async fn page_call(app: &AppHandle, kind: &str, tool: &str, args: Value) -> Value {
    let l = app.state::<Link>();
    if app.get_webview_window("main").is_none() {
        return json!({ "error": "La fenêtre de l'app n'est pas ouverte." });
    }
    let id = l.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();
    l.pending.lock().unwrap().insert(id, tx);
    let _ = app.emit_to("main", "agent-call", json!({ "id": id, "kind": kind, "tool": tool, "args": args }));
    match tokio::time::timeout(Duration::from_secs(180), rx).await {
        Ok(Ok(v)) => v,
        _ => {
            l.pending.lock().unwrap().remove(&id);
            json!({ "error": "L'interface ne répond pas." })
        }
    }
}

/// Exécute un outil (comme si l'assistant l'appelait).
pub async fn call(app: &AppHandle, tool: &str, args: Value) -> Value {
    let page = app.try_state::<Link>().map(|l| l.page_tools.iter().any(|t| t == tool)).unwrap_or(false);
    match tool {
        "show_window" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
            json!({ "ok": true })
        }
        "screenshot" => match screenshot::capture(app) {
            Ok(p) => json!({ "path": p }),
            Err(e) => json!({ "error": e }),
        },
        t if page => page_call(app, "page", t, args).await,
        t if UI_TOOLS.contains(&t) => page_call(app, "ui", t, args).await,
        t => json!({ "error": format!("Outil inconnu : {t}") }),
    }
}

async fn handle(app: AppHandle, msg: Value) -> Value {
    match msg["cmd"].as_str().unwrap_or("") {
        "ping" => json!({ "ok": true, "id": app.state::<Link>().info.id, "pid": std::process::id() }),
        "manifest" => manifest(&app.state::<Link>().info),
        "call" => call(&app, msg["tool"].as_str().unwrap_or(""), msg.get("args").cloned().unwrap_or(json!({}))).await,
        c => json!({ "error": format!("Commande inconnue : {c}") }),
    }
}

/// Écrit le manifeste et ouvre le pipe. À appeler une fois, dans `setup`.
pub fn start(app: &AppHandle, info: AppInfo) {
    let page_tools = info
        .tools
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .filter(|n| !NATIVE_TOOLS.contains(n))
        .map(String::from)
        .collect();
    let id = info.id;
    let _ = iris_store::write_json(&root().join("apps").join(format!("{id}.json")), &manifest(&info));
    app.manage(Link { info, page_tools, next: AtomicU64::new(0), pending: Mutex::new(HashMap::new()) });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let path = format!(r"\\.\pipe\iris-link-{id}");
        let mut server = match ServerOptions::new().first_pipe_instance(true).create(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[iris-link] pipe {path} : {e}");
                return;
            }
        };
        loop {
            if server.connect().await.is_err() {
                continue;
            }
            let conn = server;
            server = match ServerOptions::new().create(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[iris-link] {e}");
                    return;
                }
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let (rd, mut wr) = tokio::io::split(conn);
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = match serde_json::from_str::<Value>(&line) {
                        Ok(m) => handle(app.clone(), m).await,
                        Err(e) => json!({ "error": e.to_string() }),
                    };
                    let mut s = serde_json::to_string(&reply).unwrap_or("null".into());
                    s.push('\n');
                    if wr.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
}

// ------------------------------------------------------------ questions vers le chat de l'assistant
/// Envoie une requête JSON au pipe `iris-link-<name>` et attend une ligne de réponse.
pub async fn request(name: &str, msg: &Value, timeout: Duration) -> Result<Value, String> {
    let path = format!(r"\\.\pipe\iris-link-{name}");
    let fut = async {
        let c = ClientOptions::new().open(&path).map_err(|e| e.to_string())?;
        let (rd, mut wr) = tokio::io::split(c);
        let mut s = serde_json::to_string(msg).unwrap();
        s.push('\n');
        wr.write_all(s.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut lines = BufReader::new(rd).lines();
        let l = lines.next_line().await.map_err(|e| e.to_string())?.ok_or("Connexion fermée")?;
        serde_json::from_str(&l).map_err(|_| "Réponse invalide".to_string())
    };
    tokio::time::timeout(timeout, fut).await.map_err(|_| "Timeout".to_string())?
}

/// Ouvre le chat de l'assistant et y envoie (`send`) ou pré-remplit la question ; lance l'assistant s'il est fermé.
pub async fn ask(app: &AppHandle, text: &str, send: bool) -> Value {
    let (id, name) = app.try_state::<Link>().map(|l| (l.info.id, l.info.name)).unwrap_or(("", ""));
    let msg = json!({ "cmd": "ask", "text": text, "send": send, "from": { "id": id, "name": name } });
    if let Ok(v) = request("assistant", &msg, Duration::from_secs(5)).await {
        return v;
    }
    let a = iris_store::read_json(&root().join("assistant.json"));
    let Some(l) = a.as_ref().and_then(|a| a.get("launch")) else {
        return json!({ "error": "Orbiz n'est pas lancé." });
    };
    let mut cmd = std::process::Command::new(l["command"].as_str().unwrap_or(""));
    if let Some(args) = l["args"].as_array() {
        cmd.args(args.iter().filter_map(|a| a.as_str()));
    }
    if let Some(c) = l["cwd"].as_str() {
        cmd.current_dir(c);
    }
    cmd.env_remove("ELECTRON_RUN_AS_NODE");
    let _ = cmd.spawn();
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(v) = request("assistant", &msg, Duration::from_secs(5)).await {
            return v;
        }
    }
    json!({ "error": "Orbiz ne répond pas." })
}

/// Commandes Tauri prêtes à l'emploi :
/// `generate_handler![iris_link::commands::agent_reply, iris_link::commands::assistant_ask]`.
pub mod commands {
    use serde_json::Value;
    use tauri::AppHandle;

    /// `invoke('agent_reply', { id, result })`
    #[tauri::command]
    pub fn agent_reply(app: AppHandle, id: u64, result: Value) {
        super::reply(&app, id, result);
    }

    /// `invoke('assistant_ask', { text, send })`
    #[tauri::command]
    pub async fn assistant_ask(app: AppHandle, text: String, send: Option<bool>) -> Value {
        super::ask(&app, &text, send.unwrap_or(true)).await
    }
}
