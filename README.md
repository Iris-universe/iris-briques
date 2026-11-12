# iris-briques

Briques Rust réutilisables pour les apps desktop Iris (Rust + Tauri 2, Windows).

| Crate | Rôle |
|---|---|
| [`iris-decode`](crates/iris-decode) | Décode MP3, AAC/M4A, FLAC, WAV, OGG et AIFF en PCM stéréo `f32` (symphonia), puis rééchantillonne (rubato, FFT) |
| [`iris-store`](crates/iris-store) | JSON lu et écrit de façon atomique, dossiers de données (`%APPDATA%`, Documents, Musique), ouverture d'un dossier ou d'une URL |
| [`iris-link`](crates/iris-link) | Rend une app Tauri pilotable par un assistant IA via un named pipe (`\\.\pipe\iris-link-<id>`), avec un manifeste d'outils, l'exécution des outils dans la page (`window.irisAgent`) et une capture de la fenêtre en PNG |

## Utilisation

```toml
[dependencies]
iris-decode = { git = "https://github.com/Iris-universe/iris-briques" }
iris-link   = { git = "https://github.com/Iris-universe/iris-briques" }
```

Pour développer en local, on peut aussi pointer vers une copie du dépôt : `{ path = "../iris-briques/crates/iris-link" }`.

### iris-link

```rust
fn tools() -> serde_json::Value {
    let obj = iris_link::obj;
    serde_json::json!([
        { "name": "get_state", "description": "État de l'écran", "parameters": obj(json!({}), json!([])) },
        { "name": "screenshot", "description": "Capture PNG", "parameters": obj(json!({}), json!([])) },
    ])
}

tauri::Builder::default()
    .invoke_handler(tauri::generate_handler![
        iris_link::commands::agent_reply,
        iris_link::commands::assistant_ask,
    ])
    .setup(|app| {
        iris_link::start(app.handle(), iris_link::AppInfo {
            id: "monapp", name: "Mon App",
            description: "Ce que fait l'app.",
            instructions: "Consignes pour l'assistant.",
            tools: tools(),
        });
        Ok(())
    })
```

Les outils déclarés, sauf `screenshot` et `show_window`, sont exécutés dans l'interface. Rust émet l'évènement `agent-call` `{ id, kind, tool, args }` à destination de la fenêtre `main`. La page appelle alors `window.irisAgent[tool](args)` et renvoie le résultat avec `invoke('agent_reply', { id, result })`. Quand `kind` vaut `"ui"`, il s'agit d'un outil générique (`get_screen`, `click`, `type_text`, `press_key`, `scroll`, `list_elements`, `execute_javascript`), exécuté par le script de pont de la page.

## Licence

MIT
