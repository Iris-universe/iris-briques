//! Côté ASSISTANT d'iris-link (utilisé par Orbiz) : découvre les apps, expose leurs outils au modèle
//! (format OpenAI Responses), les appelle (en lançant l'app si besoin) et reçoit les questions des apps.
//! Même protocole que la version Node (`iris-link/assistant.js`, `registry.js`, `pipe.js`).

use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ServerOptions;

/// Séparateur entre l'id de l'app et le nom de l'outil : `irisdj__get_state`.
pub const SEP: &str = "__";

const UI_DEFS: &[(&str, &str, &str)] = &[
    ("app_get_screen", "Lit le texte visible de la fenêtre d'une app.", r#"{"max_chars":{"type":"integer"}}"#),
    ("app_list_elements", "Liste les éléments cliquables / champs visibles d'une app, avec leur index.", "{}"),
    ("app_click", "Clique dans une app un élément par index (de app_list_elements), sélecteur CSS ou texte visible.",
        r#"{"index":{"type":"integer"},"selector":{"type":"string"},"text":{"type":"string"}}"#),
    ("app_type_text", "Tape du texte dans un champ d'une app (index, sélecteur ou texte du champ).",
        r#"{"index":{"type":"integer"},"selector":{"type":"string"},"text":{"type":"string"},"clear":{"type":"boolean"},"submit":{"type":"boolean","description":"Appuyer sur Entrée après"}}"#),
    ("app_press_key", "Appuie sur une touche dans une app (Enter, Escape, Tab, F, …).", r#"{"key":{"type":"string"}}"#),
    ("app_scroll", "Fait défiler la fenêtre d'une app.", r#"{"direction":{"type":"string","enum":["up","down","top","bottom"]}}"#),
    ("app_execute_javascript", "Exécute du JavaScript dans la page de l'app (dernier recours si aucun outil dédié ne convient). Renvoie la valeur de la dernière expression ; utilise (async()=>{…})() pour await.",
        r#"{"code":{"type":"string"}}"#),
    ("app_show_window", "Met la fenêtre d'une app au premier plan (la lance si besoin).", "{}"),
];

fn ui_target(name: &str) -> Option<&'static str> {
    Some(match name {
        "app_get_screen" => "get_screen",
        "app_list_elements" => "list_elements",
        "app_click" => "click",
        "app_type_text" => "type_text",
        "app_press_key" => "press_key",
        "app_scroll" => "scroll",
        "app_execute_javascript" => "execute_javascript",
        "app_show_window" => "show_window",
        _ => return None,
    })
}

// ------------------------------------------------------------ annuaire
pub fn root() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("IrisLink")
}

/// Manifestes des apps (`%APPDATA%\IrisLink\apps\*.json`).
pub fn list_manifests() -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(root().join("apps")) else { return vec![] };
    let mut v: Vec<Value> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| iris_store::read_json(&p))
        .filter(|m| m["id"].is_string())
        .collect();
    v.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    v
}

/// Lance une app (exe, ou app Electron) sans hériter de ELECTRON_RUN_AS_NODE.
pub fn launch(l: &Value) -> bool {
    let Some(cmd) = l["command"].as_str().filter(|c| !c.is_empty()) else { return false };
    let mut c = std::process::Command::new(cmd);
    if let Some(args) = l["args"].as_array() {
        c.args(args.iter().filter_map(|a| a.as_str()));
    }
    if let Some(cwd) = l["cwd"].as_str().filter(|c| !c.is_empty()) {
        c.current_dir(cwd);
    }
    c.env_remove("ELECTRON_RUN_AS_NODE");
    c.spawn().is_ok()
}

pub async fn is_running(id: &str) -> bool {
    crate::request(id, &json!({ "cmd": "ping" }), Duration::from_millis(1500)).await.map(|v| v["ok"] == json!(true)).unwrap_or(false)
}

// ------------------------------------------------------------ outils pour le modèle
/// Définitions d'outils au format OpenAI Responses (`type: function`).
pub fn tool_defs() -> Vec<Value> {
    let apps = list_manifests();
    if apps.is_empty() {
        return vec![];
    }
    let ids: Vec<Value> = apps.iter().map(|a| a["id"].clone()).collect();
    let mut defs: Vec<Value> = UI_DEFS
        .iter()
        .map(|(name, desc, props)| {
            let mut p: serde_json::Map<String, Value> = serde_json::from_str(props).unwrap_or_default();
            p.insert("app".into(), json!({ "type": "string", "enum": ids, "description": "Identifiant de l'app" }));
            json!({ "type": "function", "name": name, "description": desc, "parameters": { "type": "object", "properties": p, "required": ["app"] } })
        })
        .collect();
    for a in &apps {
        for t in a["tools"].as_array().cloned().unwrap_or_default() {
            let name: String = format!("{}{SEP}{}", a["id"].as_str().unwrap_or(""), t["name"].as_str().unwrap_or("")).chars().take(64).collect();
            defs.push(json!({
                "type": "function", "name": name,
                "description": format!("[{}] {}", a["name"].as_str().unwrap_or(""), t["description"].as_str().unwrap_or("")),
                "parameters": t["parameters"],
            }));
        }
    }
    defs
}

/// Section du prompt système qui présente les apps (ouvertes ou non) et leurs consignes.
pub async fn prompt_section() -> String {
    let apps = list_manifests();
    if apps.is_empty() {
        return String::new();
    }
    let mut lines = vec![];
    for a in &apps {
        let id = a["id"].as_str().unwrap_or("");
        let on = is_running(id).await;
        let mut l = format!(
            "### {} (app=\"{id}\", {})\n{}",
            a["name"].as_str().unwrap_or(id),
            if on { "ouverte" } else { "fermée : elle se lance toute seule au premier outil" },
            a["description"].as_str().unwrap_or("")
        );
        if let Some(i) = a["instructions"].as_str().filter(|i| !i.is_empty()) {
            l.push('\n');
            l.push_str(i);
        }
        lines.push(l);
    }
    format!(
        "\n\n## Apps de l'utilisateur que tu peux piloter\nOutils dédiés : <app>{SEP}<outil>. Outils génériques (app_get_screen, app_click, …) avec le paramètre app. \
Préfère toujours les outils dédiés ; n'utilise les génériques que si aucun ne convient. \
Quand un message commence par [Depuis <app>], l'utilisateur te parle depuis cette app : commence par lire son état (get_state si disponible).\n\n{}",
        lines.join("\n\n")
    )
}

/// Ce nom d'outil est-il un outil d'app ?
pub fn handles(name: &str) -> bool {
    ui_target(name).is_some() || name.contains(SEP)
}

async fn call_app(id: &str, tool: &str, args: Value) -> Value {
    let msg = json!({ "cmd": "call", "tool": tool, "args": args });
    match crate::request(id, &msg, Duration::from_secs(30)).await {
        Ok(v) => return v,
        Err(e) if e == "Timeout" => return json!({ "error": format!("L'app {id} n'a pas répondu à temps.") }),
        Err(_) => {}
    }
    let Some(m) = list_manifests().into_iter().find(|a| a["id"].as_str() == Some(id)) else {
        return json!({ "error": format!("App inconnue ou jamais lancée : {id}") });
    };
    if !launch(&m["launch"]) {
        return json!({ "error": format!("App inconnue ou jamais lancée : {id}") });
    }
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if is_running(id).await {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(800)).await; // laisse la page finir de charger
    crate::request(id, &msg, Duration::from_secs(30)).await.unwrap_or_else(|_| json!({ "error": format!("Impossible de lancer l'app {id}.") }))
}

/// Exécute un outil d'app (`app_click`… ou `<id>__<outil>`).
pub async fn execute(name: &str, args: Value) -> Value {
    if let Some(tool) = ui_target(name) {
        let mut a = args.as_object().cloned().unwrap_or_default();
        let app = a.remove("app").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        return call_app(&app, tool, Value::Object(a)).await;
    }
    match name.split_once(SEP) {
        Some((id, tool)) => call_app(id, tool, args).await,
        None => json!({ "error": format!("Outil inconnu : {name}") }),
    }
}

/// Met une app au premier plan, ou la lance.
pub async fn open(id: &str) -> bool {
    if is_running(id).await {
        call_app(id, "show_window", json!({})).await;
        return true;
    }
    list_manifests().into_iter().find(|a| a["id"].as_str() == Some(id)).map(|m| launch(&m["launch"])).unwrap_or(false)
}

/// Serveur de l'assistant (`\\.\pipe\iris-link-assistant`) : les apps y envoient `{ cmd: "ask", text, send, from }`.
/// Écrit aussi `assistant.json` (comment relancer l'assistant) avec l'exécutable courant.
pub fn listen<F, Fut>(on_ask: F)
where
    F: Fn(Value) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let exe = std::env::current_exe().unwrap_or_default();
    let cwd = exe.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    let _ = iris_store::write_json(
        &root().join("assistant.json"),
        &json!({ "launch": { "command": exe.to_string_lossy(), "args": [], "cwd": cwd }, "updated": chrono::Utc::now().to_rfc3339() }),
    );
    serve("assistant", move |msg| {
        let on_ask = on_ask.clone();
        async move {
            match msg["cmd"].as_str().unwrap_or("") {
                "ping" => json!({ "ok": true }),
                "ask" => {
                    on_ask(msg).await;
                    json!({ "ok": true })
                }
                c => json!({ "error": format!("Commande inconnue : {c}") }),
            }
        }
    });
}

/// Serveur générique : une réponse JSON par ligne reçue sur `\\.\pipe\iris-link-<name>`.
pub fn serve<F, Fut>(name: &str, handler: F)
where
    F: Fn(Value) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Value> + Send + 'static,
{
    let path = format!(r"\\.\pipe\iris-link-{name}");
    tauri::async_runtime::spawn(async move {
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
                Err(_) => return,
            };
            let h = handler.clone();
            tauri::async_runtime::spawn(async move {
                let (rd, mut wr) = tokio::io::split(conn);
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = match serde_json::from_str::<Value>(&line) {
                        Ok(m) => h(m).await,
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
