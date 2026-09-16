use nectra::{
    config::Config,
    models::{Provider, Registry},
};
use std::fs;

fn entry(id: &str, provider: &str, runtime: &str) -> String {
    format!(
        "[[model]]\nid = \"{id}\"\nprovider = \"{provider}\"\nruntime = \"{runtime}\"\n\
         enabled = true\neffort = \"\"\n[model.capabilities]\ncoding=0.8\nreasoning=0.8\n\
         long_context=0.8\nwriting=0.8\ntool_use=0.8\nspeed=0.8\ncost_efficiency=0.8\n\n"
    )
}

/// A registry holding the original bundled ids, deliberately without `haiku`.
fn registry_without_haiku() -> String {
    [
        entry("opus", "anthropic", "claude"),
        entry("sonnet", "anthropic", "claude"),
        entry("gpt-5.6-sol", "openai", "codex"),
        entry("gpt-5.6-luna", "openai", "codex"),
    ]
    .concat()
}

fn load(registry_toml: &str, providers: Option<&str>) -> (tempfile::TempDir, Registry) {
    let dir = tempfile::tempdir().unwrap();
    // A top-level key has to precede the first table, or TOML reads it as a
    // member of whichever table it was appended after.
    let mut config = String::new();
    if let Some(list) = providers {
        config.push_str(&format!("providers = [{list}]\n"));
    }
    config.push_str(include_str!("../config/nectra.toml"));
    fs::write(dir.path().join("config.toml"), config).unwrap();
    fs::write(dir.path().join("models.toml"), registry_toml).unwrap();
    let cfg = Config::from_path(&dir.path().join("config.toml")).unwrap();
    let registry = Registry::load(&cfg).unwrap();
    (dir, registry)
}

#[test]
fn a_model_removed_from_the_registry_stays_removed() {
    let (_dir, registry) = load(&registry_without_haiku(), None);
    assert!(
        !registry.models.iter().any(|m| m.id == "haiku"),
        "haiku was re-injected into a registry the user had removed it from"
    );
}

#[test]
fn the_bundled_registry_declares_gemini_like_every_other_model() {
    let (_dir, registry) = load(include_str!("../config/models.toml"), None);
    assert!(
        registry
            .models
            .iter()
            .any(|m| m.provider == Provider::Google),
        "Gemini profiles should live in config/models.toml, not be built in Rust"
    );
}

#[test]
fn selecting_google_enables_the_declared_gemini_models() {
    let (_dir, registry) = load(include_str!("../config/models.toml"), Some("\"google\""));
    assert!(
        registry
            .models
            .iter()
            .any(|m| m.provider == Provider::Google && m.enabled),
        "selecting google should enable the Gemini entries"
    );
}

#[test]
fn a_registry_predating_declared_roles_is_reported() {
    // An installed registry shadows the bundled one. Without a notice, routing
    // silently stays on the legacy id table with no way to tell from the CLI.
    let (_dir, registry) = load(&registry_without_haiku(), None);
    let notice = nectra::models::legacy_registry_notice(&registry)
        .expect("a roleless registry should be reported");
    assert!(
        notice.contains("prefers"),
        "notice should name the missing field: {notice}"
    );
}

#[test]
fn a_current_registry_reports_nothing() {
    let (_dir, registry) = load(include_str!("../config/models.toml"), None);
    assert!(nectra::models::legacy_registry_notice(&registry).is_none());
}
