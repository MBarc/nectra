use crate::{config::Config, executor, models::Registry};
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
};

pub struct ProviderChoice {
    pub id: &'static str,
    pub name: &'static str,
    pub connection: &'static str,
}
macro_rules! provider {
    ($id:literal, $name:literal, $connection:literal) => {
        ProviderChoice {
            id: $id,
            name: $name,
            connection: $connection,
        }
    };
    ($id:literal, $name:literal) => {
        provider!($id, $name, "not yet implemented")
    };
}
pub const PROVIDERS: &[ProviderChoice] = &[
    provider!(
        "anthropic",
        "Anthropic / Claude",
        "requires Claude Code CLI"
    ),
    provider!("openai", "OpenAI", "requires Codex CLI"),
    provider!(
        "google",
        "Google / Gemini",
        "requires Antigravity CLI (agy)"
    ),
    provider!(
        "ollama",
        "Ollama (local or your own server)",
        "requires a running Ollama server (chat only)"
    ),
    // Not offered by setup: kept so configurations that saved them still load.
    provider!("xai", "xAI / Grok"),
    provider!("mistral", "Mistral AI"),
    provider!("deepseek", "DeepSeek"),
    provider!("cohere", "Cohere"),
    provider!("groq", "Groq"),
    provider!("together", "Together AI"),
    provider!("fireworks", "Fireworks AI"),
    provider!("perplexity", "Perplexity"),
    provider!("openrouter", "OpenRouter"),
    provider!("azure", "Microsoft Azure"),
    provider!("bedrock", "Amazon Bedrock"),
    provider!("huggingface", "Hugging Face"),
    provider!("lmstudio", "LM Studio (local)"),
    provider!("custom", "Other / custom endpoint"),
];

pub fn parse_providers(value: &str) -> Result<Vec<String>> {
    if value.trim().eq_ignore_ascii_case("none") {
        return Ok(vec![]);
    }
    let mut selected = vec![];
    for token in value.split(',').map(str::trim) {
        let lower = token.to_ascii_lowercase();
        let choice = token
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| PROVIDERS.get(n))
            .or_else(|| PROVIDERS.iter().find(|p| p.id == lower))
            .with_context(|| {
                format!(
                    "Unknown provider '{token}'. Enter comma-separated numbers or IDs, or 'none'."
                )
            })?;
        if !selected.iter().any(|p| p == choice.id) {
            selected.push(choice.id.to_string());
        }
    }
    Ok(selected)
}

pub fn manage(path: Option<&Path>, command: &crate::cli::ProviderCommands) -> Result<()> {
    use crate::cli::ProviderCommands;
    let path = path
        .map(Path::to_path_buf)
        .or_else(|| dirs::config_dir().map(|p| p.join("nectra/config.toml")))
        .context("cannot find user configuration directory; pass --config")?;
    let exists = path.try_exists()?;
    let cfg = if exists {
        Config::from_path(&path)?
    } else {
        Config::from_defaults()?
    };
    let mut selected = cfg
        .inner
        .providers
        .clone()
        .unwrap_or_else(|| vec!["anthropic".into(), "openai".into()]);
    let id = match command {
        ProviderCommands::Login { id } => return crate::executor::login(id, &cfg),
        ProviderCommands::List => {
            let mut output = crate::terminal::stdout();
            for provider in PROVIDERS {
                writeln!(
                    output,
                    "{:<14} {:<12} {} — {}",
                    provider.id,
                    if selected.iter().any(|p| p == provider.id) {
                        "selected"
                    } else {
                        "not selected"
                    },
                    provider.name,
                    provider.connection
                )?;
            }
            writeln!(output,"Selection does not verify authentication. Changes apply to newly started chats; restart/resume an open chat to reload configuration.")?;
            return Ok(());
        }
        ProviderCommands::Add { id } | ProviderCommands::Remove { id } => id.to_ascii_lowercase(),
    };
    if !PROVIDERS.iter().any(|p| p.id == id) {
        bail!("unknown provider '{id}'; run nectra provider list for IDs");
    }
    match command {
        ProviderCommands::Add { .. } => {
            if !exists {
                selected.clear();
            }
            if !selected.contains(&id) {
                selected.push(id);
            }
        }
        ProviderCommands::Remove { .. } => selected.retain(|p| p != &id),
        ProviderCommands::List | ProviderCommands::Login { .. } => unreachable!(),
    }
    let values = if selected.is_empty() {
        "none".into()
    } else {
        selected.join(",")
    };
    run(Some(&path), Some(&values), false)
}

fn validate_connections(
    cfg: &Config,
    output: &mut dyn Write,
    check: &mut dyn FnMut(&str) -> Result<PathBuf>,
) -> Result<()> {
    if cfg.ollama_enabled() {
        cfg.inner
            .ollama
            .as_ref()
            .context("Ollama selected but not configured; run nectra provider add ollama")?
            .check()?;
        writeln!(
            output,
            "[OK] Ollama server and selected local models (chat only)"
        )?;
    }
    let mut failed = false;
    for (id, name, command) in [
        ("anthropic", "Claude Code", &cfg.inner.claude.command),
        ("openai", "Codex CLI", &cfg.inner.codex.command),
        (
            "google",
            executor::google_name(cfg),
            &executor::google_config(cfg).command,
        ),
    ] {
        if id == "google" && cfg.inner.providers.is_none() {
            continue;
        }
        if cfg
            .inner
            .providers
            .as_ref()
            .is_some_and(|ids| !ids.iter().any(|p| p == id))
        {
            continue;
        }
        writeln!(
            output,
            "Checking {name} installation (Node-based CLIs may take up to 60 seconds)..."
        )?;
        output.flush()?;
        match check(command) {
            Ok(path) => writeln!(
                output,
                "[OK] {name}: {} (--version succeeded)",
                path.display()
            )?,
            Err(error) => {
                failed = true;
                writeln!(
                    output,
                    "[MISSING OR UNUSABLE] {name} ({command}): {error:#}"
                )?;
                writeln!(output, "Install/repair this CLI, or remove {id} from your selection, then run nectra setup again.")?;
            }
        }
    }
    if failed {
        bail!("provider setup incomplete; required CLI checks failed. Existing configuration was not changed");
    }
    writeln!(
        output,
        "CLI checks do not verify authentication or model access; no model requests were made."
    )?;
    Ok(())
}

pub fn run(path: Option<&Path>, providers: Option<&str>, if_needed: bool) -> Result<()> {
    let path = path
        .map(Path::to_path_buf)
        .or_else(|| dirs::config_dir().map(|p| p.join("nectra/config.toml")))
        .context("cannot find user configuration directory; pass --config")?;
    if if_needed && path.try_exists()? {
        println!(
            "Existing setup preserved: {}. Checking selected CLI dependencies.",
            path.display()
        );
        let cfg = Config::from_path(&path)?;
        if crate::jev::allowed(&cfg) && crate::jev::api_key(&cfg.path).is_none() {
            println!("Tip: run nectra setup to add a TypeSafe key for smarter, cheaper routing.");
        }
        return validate_connections(
            &cfg,
            &mut crate::terminal::stdout(),
            &mut executor::check_runtime,
        );
    }
    if providers.is_none() && !io::stdin().is_terminal() {
        bail!("interactive setup needs a terminal; run nectra setup, or pass --providers anthropic,openai (or none)");
    }
    configure(
        &path,
        providers,
        &mut io::stdin().lock(),
        &mut crate::terminal::stdout(),
        &mut executor::check_runtime,
    )?;
    // Unattended installs pass --providers and set TYPESAFE_API_KEY instead.
    if providers.is_none() && path.try_exists()? {
        jev_key_prompt(&Config::from_path(&path)?, &mut crate::terminal::stdout())?;
    }
    Ok(())
}

/// Jev runs only as TypeSafe's hosted API (no local weights), so smarter
/// routing needs a key. Skipping keeps the offline keyword classifier.
fn jev_key_prompt(cfg: &Config, output: &mut dyn Write) -> Result<()> {
    use crate::jev;
    if !jev::allowed(cfg) {
        return Ok(());
    }
    if std::env::var(jev::KEY_ENV).is_ok_and(|k| !k.trim().is_empty()) {
        writeln!(
            output,
            "Jev routing: using {} from the environment.",
            jev::KEY_ENV
        )?;
        return Ok(());
    }
    let saved = jev::api_key(&cfg.path).is_some();
    writeln!(
        output,
        "
Smarter routing (optional): Nectra can classify each message with Jev, TypeSafe's          hosted classifier (~0.2 s and a fraction of a cent per message), so easy prompts go to          cheaper models. Message text is sent to api.typesafe.ai. Get a key at https://typesafe.ai.          For a free classifier that runs on this machine instead, run `nectra classifier laya`."
    )?;
    loop {
        write!(
            output,
            "{}",
            if saved {
                "TypeSafe API key (hidden; Enter keeps the saved key): "
            } else {
                "TypeSafe API key (hidden; Enter skips and keeps keyword routing): "
            }
        )?;
        output.flush()?;
        let key = read_hidden()?;
        writeln!(output)?;
        if key.is_empty() {
            writeln!(
                output,
                "{}",
                if saved {
                    "Kept the saved Jev key."
                } else {
                    "Skipped. Run nectra setup again to add a key later."
                }
            )?;
            return Ok(());
        }
        match jev::verify(&key) {
            Ok(()) => {
                let path = jev::save_key(&cfg.path, &key)?;
                writeln!(output, "Key verified and saved to {}.", path.display())?;
                return Ok(());
            }
            Err(e) => writeln!(
                output,
                "Key check failed: {e:#}. Try again, or press Enter to skip."
            )?,
        }
    }
}

/// Reads a line without echoing it; Esc or Ctrl+C returns empty.
fn read_hidden() -> Result<String> {
    use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
    crossterm::terminal::enable_raw_mode()?;
    let mut key = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Esc => {
                    key.clear();
                    break Ok(());
                }
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    key.clear();
                    break Ok(());
                }
                KeyCode::Char(c) => key.push(c),
                KeyCode::Backspace => {
                    key.pop();
                }
                _ => {}
            },
            Ok(Event::Paste(text)) => key.push_str(&text),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    result?;
    Ok(key.trim().to_string())
}

pub fn configure(
    path: &Path,
    providers: Option<&str>,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    check: &mut dyn FnMut(&str) -> Result<PathBuf>,
) -> Result<()> {
    let exists = path.try_exists()?;
    let mut cfg = if exists {
        Config::from_path(path)?
    } else {
        Config::from_defaults()?
    };
    let selected = if let Some(value) = providers {
        parse_providers(value)?
    } else {
        writeln!(
            output,
            "Which providers do you have access to? Select all that apply."
        )?;
        // Only providers that can answer are offered; they come first, so the
        // numbers shown are the numbers parse_providers accepts.
        for (i, provider) in PROVIDERS
            .iter()
            .enumerate()
            .filter(|(_, p)| p.connection != "not yet implemented")
        {
            writeln!(
                output,
                "{:>2}. {} [{}] — {}",
                i + 1,
                provider.name,
                provider.id,
                provider.connection
            )?;
        }
        if let Some(current) = &cfg.inner.providers {
            writeln!(output, "Current selection: {}", current.join(", "))?;
        }
        loop {
            write!(
                output,
                "Numbers or IDs separated by commas (none disables all; blank cancels): "
            )?;
            output.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 || line.trim().is_empty() {
                writeln!(output, "Setup cancelled; configuration unchanged.")?;
                return Ok(());
            }
            match parse_providers(line.trim()) {
                Ok(value) => break value,
                Err(error) => writeln!(output, "{error}")?,
            }
        }
    };
    for id in &selected {
        let provider = PROVIDERS.iter().find(|p| p.id == id).unwrap();
        writeln!(output, "{}: {}", provider.name, provider.connection)?;
    }
    if !selected
        .iter()
        .any(|id| id == "openai" || id == "anthropic" || id == "google" || id == "ollama")
    {
        writeln!(output, "No executable provider selected in this build. Chat needs a supported connection before it can answer.")?;
    }
    cfg.inner.providers = Some(selected);
    if cfg.ollama_enabled() && cfg.inner.ollama.is_none() {
        cfg.inner.ollama = Some(crate::ollama::wizard(input, output)?);
    }
    validate_connections(&cfg, output, check)?;
    save_config(path, cfg, output)
}

fn save_config(path: &Path, mut cfg: Config, output: &mut dyn Write) -> Result<()> {
    let exists = path.try_exists()?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    if !exists {
        let registry = directory.join("models.toml");
        if !registry.try_exists()? {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&registry)?;
            file.write_all(include_bytes!("../config/models.toml"))?;
            file.sync_all()?;
        }
    }
    cfg.path = path.to_path_buf();
    cfg.embedded = false;
    Registry::load(&cfg)?.validate()?;
    let text = cfg.pretty_toml()?;
    let temporary = directory.join(format!(".nectra-setup-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path).context("saving provider configuration")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    writeln!(
        output,
        "Saved {}. Unselected providers are excluded from routing and fallback.",
        path.display()
    )?;
    writeln!(output, "Run nectra setup to change this selection later.")?;
    Ok(())
}

pub fn classifier_setup(
    path: Option<&Path>,
    backend: crate::config::ClassifierBackend,
    model_dir: Option<&Path>,
    verify_only: bool,
) -> Result<()> {
    if (model_dir.is_some() || verify_only) && backend != crate::config::ClassifierBackend::Laya {
        bail!("--model-dir and --verify-only apply to `nectra classifier laya`");
    }
    let path = path
        .map(Path::to_path_buf)
        .or_else(|| dirs::config_dir().map(|p| p.join("nectra/config.toml")))
        .context("cannot find config directory")?;
    let mut cfg = if path.try_exists()? {
        Config::from_path(&path)?
    } else {
        Config::from_defaults()?
    };
    if let Some(dir) = model_dir {
        cfg.inner.classifier.model_dir = Some(dir.to_path_buf());
    }
    let mut output = crate::terminal::stdout();
    if verify_only {
        let dir = crate::laya::model_dir(&cfg).context("cannot find a data directory")?;
        return crate::laya::verify_dir(&dir, &mut output);
    }
    if backend == crate::config::ClassifierBackend::Laya {
        crate::laya::download(&cfg, &mut output)?;
    }
    cfg.inner.classifier.backend = backend;
    save_config(&path, cfg, &mut output)
}

pub fn ollama_setup(
    path: Option<&Path>,
    url: &str,
    names: &[String],
    policy: crate::ollama::LocalPolicy,
) -> Result<()> {
    let path = path
        .map(Path::to_path_buf)
        .or_else(|| dirs::config_dir().map(|p| p.join("nectra/config.toml")))
        .context("cannot find config directory")?;
    let exists = path.try_exists()?;
    let mut cfg = if exists {
        Config::from_path(&path)?
    } else {
        Config::from_defaults()?
    };
    let ollama = crate::ollama::configure(url, names, policy)?;
    let mut providers = cfg.inner.providers.clone().unwrap_or_else(|| {
        if exists {
            vec!["anthropic".into(), "openai".into()]
        } else {
            vec![]
        }
    });
    if !providers.iter().any(|p| p == "ollama") {
        providers.push("ollama".into());
    }
    cfg.inner.providers = Some(providers);
    cfg.inner.ollama = Some(ollama);
    save_config(&path, cfg, &mut crate::terminal::stdout())
}
