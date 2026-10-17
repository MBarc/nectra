use crate::{
    cli::{Cli, Commands, Mode, ProviderArg},
    config::Config,
    executor,
    models::{Registry, Runtime},
    router::{self, Decision},
    session::{Session, CONTEXT_BYTES},
};
use anyhow::{bail, Result};
use clap::ValueEnum;
use std::io::{self, BufRead, Write};

const HELP: &str = "/help                 Show commands\n/models               Show enabled models and availability\n/model ID | auto      Pin a model or route each turn automatically\n/again [ID]           Send your last message again to the best other model, or to ID\n/provider NAME | auto anthropic, openai, google, ollama, or auto\n/theme NAME           default, hacker, or ice-blue (saved)\n/login PROVIDER       Reauthenticate through the provider CLI\n/mode fast|balanced|best\n/debate on|off        Toggle isolated two-provider comparison\n/history              Show this conversation\n/session              Show the saved session path\n/clear                Start fresh context in this saved session\n/paste                Multiline message; finish with /send, discard with /cancel\n/quit                 Save and exit (EOF also exits)\n";

pub const BANNER: &str = r"
 _   _ _____ ____ _____ ____      _
| \ | | ____/ ___|_   _|  _ \    / \
|  \| |  _|| |     | | | |_) |  / _ \
| |\  | |__| |___  | | |  _ <  / ___ \
|_| \_|_____\____| |_| |_| \_\/_/   \_\
";

pub trait Backend {
    fn take_queued_messages(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn login(&mut self, _provider: &str, _cfg: &Config) -> Result<()> {
        bail!("login is not supported by this backend")
    }
    fn available(&self, runtime: Runtime, cfg: &Config) -> bool;
    fn respond(
        &mut self,
        decision: &Decision,
        prompt: &str,
        cfg: &Config,
        output: &mut dyn Write,
    ) -> Result<String>;
    /// `respond`, keeping or continuing a provider session when one is given.
    /// Backends without session support answer statelessly and clear the id,
    /// so the next turn replays the transcript instead of resuming nothing.
    fn respond_session(
        &mut self,
        decision: &Decision,
        prompt: &str,
        cfg: &Config,
        output: &mut dyn Write,
        session: Option<&mut executor::ProviderSession>,
    ) -> Result<String> {
        if let Some(s) = session {
            s.id = None;
        }
        self.respond(decision, prompt, cfg, output)
    }
    /// One quiet resumed turn on the outgoing model; its reply is not shown or saved.
    fn handoff(
        &mut self,
        decision: &Decision,
        request: &str,
        cfg: &Config,
        session: &mut executor::ProviderSession,
    ) -> Result<String> {
        self.respond_session(decision, request, cfg, &mut Vec::new(), Some(session))
    }
}
pub struct NativeBackend;
impl Backend for NativeBackend {
    fn take_queued_messages(&mut self) -> Vec<String> {
        executor::native::take_queued_messages()
    }
    fn login(&mut self, provider: &str, cfg: &Config) -> Result<()> {
        executor::login(provider, cfg)
    }
    fn available(&self, runtime: Runtime, cfg: &Config) -> bool {
        let command = match runtime {
            Runtime::Claude => &cfg.inner.claude.command,
            Runtime::Codex => &cfg.inner.codex.command,
            Runtime::Ollama => return cfg.inner.ollama.is_some(),
            Runtime::Gemini => &cfg.inner.gemini.command,
        };
        executor::launch(command).is_ok()
    }
    fn respond(
        &mut self,
        decision: &Decision,
        prompt: &str,
        cfg: &Config,
        output: &mut dyn Write,
    ) -> Result<String> {
        if decision.runtime == "ollama" {
            crate::ollama::conversation(&decision.model, prompt, cfg, output)
        } else if executor::native::enabled(cfg, &decision.runtime)? {
            executor::native::conversation(decision, prompt, cfg)
        } else {
            executor::conversation(decision, prompt, cfg, output)
        }
    }
    fn respond_session(
        &mut self,
        decision: &Decision,
        prompt: &str,
        cfg: &Config,
        output: &mut dyn Write,
        session: Option<&mut executor::ProviderSession>,
    ) -> Result<String> {
        if session.is_none() || decision.runtime == "ollama" {
            if let Some(s) = session {
                s.id = None;
            }
            self.respond(decision, prompt, cfg, output)
        } else if executor::native::enabled(cfg, &decision.runtime)? {
            executor::native::conversation_with_session(decision, prompt, cfg, session)
        } else {
            executor::conversation_resumable(decision, prompt, cfg, output, session)
        }
    }
    fn handoff(
        &mut self,
        decision: &Decision,
        request: &str,
        cfg: &Config,
        session: &mut executor::ProviderSession,
    ) -> Result<String> {
        // Always the quiet stream view: no provider terminal flashes up for a note.
        // A note needs no writes, whatever the working agent was allowed.
        let cfg = &Config {
            read_only: true,
            ..cfg.clone()
        };
        let activity = crate::activity::Activity::start(&decision.model);
        let mut sink = Vec::new();
        let mut progress = activity.writer(&mut sink);
        executor::conversation_resumable(decision, request, cfg, &mut progress, Some(session))
    }
}
pub struct ChatOptions {
    /// Where cooldowns are remembered, or None to keep them in memory only.
    /// Injected rather than resolved inside the loop so a test run neither
    /// reads nor writes the real user state.
    pub cooldown_state: Option<std::path::PathBuf>,
    pub mode: Mode,
    pub provider: Option<ProviderArg>,
    pub model: Option<String>,
    pub explain: bool,
    pub dry_run: bool,
}
pub fn run(cli: &Cli, cfg: &Config, registry: &Registry) -> Result<i32> {
    if cli.stdin {
        bail!(
            "chat reads one message at a time; omit --stdin and use /paste for multiline messages"
        );
    }
    let cwd = std::env::current_dir()?;
    let explicit = matches!(
        &cli.command,
        Some(
            Commands::Chat {
                resume: Some(_),
                ..
            } | Commands::Chat {
                session: Some(_),
                ..
            }
        )
    );
    if cli.continue_latest && explicit {
        bail!("use --continue, --resume or --session, not more than one");
    }
    let latest = if cli.continue_latest {
        let latest = Session::latest_default(&cwd)?;
        if latest.is_none() {
            crate::terminal::line(
                "No earlier conversation in this directory; starting a new one.",
            )?;
        }
        latest
    } else {
        None
    };
    let mut session = match &cli.command {
        _ if latest.is_some() => Session::resume(latest.as_deref().unwrap(), &cwd)?,
        Some(Commands::Chat {
            resume: Some(path), ..
        }) => Session::resume(path, &cwd)?,
        Some(Commands::Chat {
            session: Some(path),
            ..
        }) => Session::create(path, &cwd)?,
        _ => Session::create_default(&cwd)?,
    };
    let options = ChatOptions {
        cooldown_state: crate::limits::Cooldowns::state_path(),
        mode: cli.mode.unwrap_or(cfg.inner.routing.default_mode),
        provider: cli.provider,
        model: cli.model.clone(),
        explain: cli.explain,
        dry_run: cli.dry_run,
    };
    run_loop(
        cfg,
        registry,
        options,
        &mut session,
        &mut io::stdin().lock(),
        &mut crate::terminal::stdout(),
        &mut NativeBackend,
    )?;
    Ok(0)
}

pub fn run_loop(
    cfg: &Config,
    registry: &Registry,
    mut options: ChatOptions,
    session: &mut Session,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    backend: &mut dyn Backend,
) -> Result<()> {
    writeln!(
        output,
        "{BANNER}  v{}  |  One conversation. The right model.\n",
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(
        output,
        "Session: {}\nType /help for commands. Replies share this conversation's context.",
        session.path.display()
    )?;
    if options.dry_run {
        writeln!(
            output,
            "Preview mode: decisions only; no model calls or new conversation turns."
        )?;
    }
    if cfg.skip_permissions {
        writeln!(output, "Permission bypass ON for this launch: Claude/Gemini auto-approve tools; Codex also disables its sandbox. Ollama remains chat-only.")?;
    }
    if session.recovered_tail {
        writeln!(output, "Recovered the log up to its last complete record.")?;
    }
    if let Some(message) = &session.interrupted {
        writeln!(
            output,
            "An interrupted message was saved but was not rerun: {message}"
        )?;
    }
    if !session.exchanges.is_empty() {
        writeln!(
            output,
            "Resumed {} completed turns. Use /history to view them.",
            session.exchanges.len()
        )?;
    }
    // Remembered across runs: a limit learned in one session should not be
    // rediscovered by walking into it again in the next.
    let cooldown_state = options.cooldown_state.clone();
    let mut cooldowns = match &cooldown_state {
        Some(path) => crate::limits::Cooldowns::load(path),
        None => crate::limits::Cooldowns::default(),
    };
    let remember = |cooldowns: &crate::limits::Cooldowns| {
        if let Some(path) = &cooldown_state {
            // Never let bookkeeping break a conversation.
            let _ = cooldowns.persist(path);
        }
    };
    let mut needs_login: Vec<String> = vec![];
    let mut denied_models = std::collections::HashSet::<String>::new();
    let mut successful_models = std::collections::HashSet::<String>::new();
    // The provider session behind the last completed turn. Only that model can
    // resume it: any other answer since would be missing from its history.
    let mut provider_session = session.provider_session.clone();
    let mut queued = std::collections::VecDeque::new();
    let mut debate_mode = false;
    // Set by /mode: the next message routes afresh instead of staying on the
    // model the previous mode picked.
    let mut reroute = false;
    // The last message that got no saved answer, which /again sends as is.
    let mut unanswered = session.interrupted.clone().or(session.last_failed.clone());
    // Set by /again for the next turn: a model to use, or one to beat.
    let mut again: Option<Again> = None;
    'messages: loop {
        queued.extend(backend.take_queued_messages());
        write!(output, "\nyou> ")?;
        output.flush()?;
        let mut text = String::new();
        if let Some(message) = queued.pop_front() {
            text = message;
            writeln!(output, "{text} [queued]")?;
        } else if input.read_line(&mut text)? == 0 {
            writeln!(output)?;
            break;
        }
        // Windows PowerShell prefixes piped input with a byte-order mark, which
        // would otherwise hide a leading /command.
        let text = text
            .trim_start_matches('\u{feff}')
            .trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() {
            continue;
        }
        let mut message = text.to_string();
        if text.starts_with('/') {
            let (command, value) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
            let value = value.trim();
            match command {
                "/debate" => {
                    match value {
                        "on" => debate_mode = true,
                        "off" => debate_mode = false,
                        "" => debate_mode = !debate_mode,
                        _ => {
                            writeln!(output, "Use /debate on or /debate off.")?;
                            continue;
                        }
                    }
                    writeln!(output, "Debate {}. Uses isolated streams and up to five logical provider calls per turn. Requires the debate Docker image; no writable fallback.", if debate_mode {"on"} else {"off"})?;
                    continue;
                }
                "/theme" => {
                    if value.is_empty() {
                        writeln!(
                            output,
                            "Theme: {}. Available: default, hacker, ice-blue.",
                            crate::terminal::theme().name()
                        )?;
                    } else {
                        match crate::terminal::Theme::from_str(value, true) {
                            Ok(theme) => {
                                let path = crate::terminal::theme_path(
                                    (!cfg.embedded).then_some(cfg.path.as_path()),
                                )?;
                                match crate::terminal::save_theme(&path, theme) {
                                    Ok(()) => {
                                        crate::terminal::set_theme(theme);
                                        writeln!(output, "Theme: {} (saved).", theme.name())?;
                                    }
                                    Err(e) => writeln!(output, "Theme was not changed: {e:#}")?,
                                }
                            }
                            Err(_) => writeln!(
                                output,
                                "Unknown theme. Use /theme default, hacker, or ice-blue."
                            )?,
                        }
                    }
                    continue;
                }
                "/login" => {
                    let provider = match value {
                        "claude" => "anthropic",
                        "codex" => "openai",
                        "gemini" => "google",
                        p => p,
                    };
                    output.flush()?;
                    match backend.login(provider, cfg) {
                        Ok(()) => {
                            denied_models.retain(|id| {
                                !registry
                                    .models
                                    .iter()
                                    .any(|m| &m.id == id && m.provider.as_str() == provider)
                            });
                            successful_models.retain(|id| {
                                !registry
                                    .models
                                    .iter()
                                    .any(|m| &m.id == id && m.provider.as_str() == provider)
                            });
                            needs_login.retain(|p| p != provider);
                            writeln!(output,"Login workflow closed. Send a new message to verify provider access.")?;
                        }
                        Err(e) => writeln!(output, "Login failed: {e:#}")?,
                    }
                    continue;
                }
                "/quit" | "/exit" => break,
                "/help" => {
                    write!(output, "{HELP}")?;
                    continue;
                }
                "/again" => {
                    if debate_mode {
                        writeln!(
                            output,
                            "/again isn't available in debate mode; use /debate off first."
                        )?;
                        continue;
                    }
                    let usable = |id: &str| {
                        registry
                            .models
                            .iter()
                            .any(|m| m.enabled && m.id == id && backend.available(m.runtime, cfg))
                    };
                    if !value.is_empty() && !usable(value) {
                        writeln!(
                            output,
                            "Unknown, disabled or unavailable model. Use /models."
                        )?;
                        continue;
                    }
                    let (last, model) = if let Some(text) = unanswered.clone() {
                        (text, None)
                    } else if let Some(last) = session.exchanges.last().cloned() {
                        (last.user, Some(last.model))
                    } else {
                        writeln!(output, "Nothing to send again yet.")?;
                        continue;
                    };
                    again = Some(match (value, model) {
                        ("", Some(beat)) => Again::Stronger(beat),
                        ("", None) => Again::Same,
                        (id, replaces) => Again::Model(id.to_string(), replaces),
                    });
                    reroute = true;
                    message = last;
                    writeln!(output, "you> {message} [again]")?;
                }
                "/session" => {
                    writeln!(output, "{}", session.path.display())?;
                    continue;
                }
                "/history" => {
                    for exchange in &session.exchanges {
                        writeln!(
                            output,
                            "\nyou> {}\n{}> {}",
                            exchange.user, exchange.model, exchange.assistant
                        )?;
                    }
                    continue;
                }
                "/clear" => {
                    session.clear()?;
                    provider_session = None;
                    unanswered = None;
                    writeln!(
                        output,
                        "Context cleared. Earlier turns remain in the saved log."
                    )?;
                    continue;
                }
                "/models" => {
                    for model in &registry.models {
                        if needs_login.iter().any(|p| p == model.provider.as_str()) {
                            writeln!(
                                output,
                                "{} ({}) — login required; /login {}",
                                model.id,
                                model.provider.as_str(),
                                model.provider.as_str()
                            )?;
                            continue;
                        }
                        if let Some(wait) = cooldowns.remaining(&model.id, model.provider.as_str())
                        {
                            writeln!(
                                output,
                                "{} ({}) — cooling down for {}s",
                                model.id,
                                model.provider.as_str(),
                                wait.as_secs() + 1
                            )?;
                            continue;
                        }
                        let state = if denied_models.contains(&model.id) {
                            "model rejected; /model retry clears access observations"
                        } else if !model.enabled {
                            "disabled"
                        } else if successful_models.contains(&model.id) {
                            "last request succeeded (session observation)"
                        } else if backend.available(model.runtime, cfg) {
                            "CLI installed; model access unverified until used"
                        } else {
                            "runtime unavailable"
                        };
                        writeln!(
                            output,
                            "{} ({}) — {state}",
                            model.id,
                            model.provider.as_str()
                        )?;
                    }
                    writeln!(output, "CLI availability checks installation. Ollama configuration is checked here; server/model readiness is verified on use or by nectra doctor. Ollama is chat-only.")?;
                    continue;
                }
                "/model" => {
                    if value == "retry" {
                        denied_models.clear();
                        successful_models.clear();
                        writeln!(output, "Model access observations cleared. Send a message to retry; nothing was replayed.")?;
                    } else if value == "auto" {
                        options.model = None;
                        options.provider = None;
                        writeln!(output, "Automatic routing across available models.")?;
                    } else if registry
                        .models
                        .iter()
                        .any(|m| m.enabled && m.id == value && backend.available(m.runtime, cfg))
                    {
                        options.model = Some(value.into());
                        options.provider = None;
                        writeln!(output, "Model pinned to {value}.")?;
                    } else {
                        writeln!(
                            output,
                            "Unknown, disabled or unavailable model. Use /models or /model auto."
                        )?;
                    }
                    continue;
                }
                "/provider" => {
                    if value == "auto" {
                        options.provider = None;
                        options.model = None;
                        writeln!(
                            output,
                            "All selected providers allowed under your routing policy."
                        )?;
                    } else {
                        match ProviderArg::from_str(value, true) {
                            Ok(provider) => {
                                options.provider = Some(provider);
                                options.model = None;
                                writeln!(output, "Provider: {value}.")?;
                            }
                            Err(_) => writeln!(
                                output,
                                "Use /provider anthropic, openai, google, ollama or auto."
                            )?,
                        }
                    }
                    continue;
                }
                "/mode" => {
                    match Mode::from_str(value, true) {
                        Ok(mode) => {
                            options.mode = mode;
                            reroute = true;
                            writeln!(output, "Mode: {value}.")?;
                        }
                        Err(_) => writeln!(output, "Use /mode fast, balanced or best.")?,
                    }
                    continue;
                }
                "/paste" => {
                    writeln!(
                        output,
                        "Paste your message. Finish with /send or discard with /cancel."
                    )?;
                    message.clear();
                    loop {
                        let mut line = String::new();
                        if input.read_line(&mut line)? == 0 || line.trim() == "/cancel" {
                            message.clear();
                            break;
                        }
                        if line.trim() == "/send" {
                            break;
                        }
                        message.push_str(&line);
                    }
                    if message.trim().is_empty() {
                        continue;
                    }
                }
                _ => {
                    writeln!(output, "Unknown command. Use /help; /paste accepts literal slash-prefixed messages.")?;
                    continue;
                }
            }
        }
        // Read directives from this message only, never from shared history or quoted text.
        let requested_model = requested_model(&message, registry);
        let again = again.take();
        let requested_model = match &again {
            Some(Again::Model(id, _)) => Some(id.clone()),
            // Any model but the one that answered: a pin or a model named in
            // the message would only point back at it.
            Some(Again::Stronger(_)) => None,
            _ => requested_model,
        };
        let mut turn_model = requested_model.clone().or_else(|| match &again {
            Some(Again::Stronger(_)) => None,
            _ => options.model.clone(),
        });
        let mut turn_provider = if requested_model.is_some() {
            None
        } else {
            options.provider
        };
        if debate_mode {
            if options.dry_run {
                match router::route(
                    &message,
                    cfg,
                    registry,
                    options.mode,
                    turn_provider,
                    turn_model,
                ) {
                    Ok(decision) => {
                        writeln!(output, "{}", serde_json::to_string_pretty(&decision)?)?
                    }
                    Err(error) => writeln!(output, "Debate routing failed: {error:#}")?,
                }
                continue;
            }
            provider_session = None;
            let result = crate::debate::command::turn(
                session,
                cfg,
                registry,
                &message,
                crate::debate::command::Options {
                    mode: options.mode,
                    provider: turn_provider,
                    model: turn_model,
                    cooldown_state: cooldown_state.clone(),
                },
                &mut cooldowns,
                output,
            );
            if let Err(error) = result {
                writeln!(output, "Debate stopped: {error:#}")?;
            }
            continue;
        }
        let mut attempted = Vec::new();
        // Taken (once) when the retry is routed; see below.
        let mut replaces = match &again {
            Some(Again::Stronger(model)) => Some(model.clone()),
            Some(Again::Model(_, replaces)) => replaces.clone(),
            _ => None,
        };
        // "/again" alone: anything but the model that answered, judged on capability.
        let mut turn_mode = options.mode;
        if let Some(Again::Stronger(beat)) = &again {
            attempted.push(beat.clone());
            turn_mode = Mode::Best;
        }
        if let Some(id) = &turn_model {
            if registry.models.iter().any(|m| {
                &m.id == id
                    && (!backend.available(m.runtime, cfg)
                        || denied_models.contains(id)
                        || needs_login.iter().any(|p| p == m.provider.as_str())
                        || cooldowns.remaining(id, m.provider.as_str()).is_some())
            }) {
                writeln!(
                    output,
                    "Requested model {id} is unavailable; selecting another model for this task."
                )?;
                turn_model = None;
                turn_provider = None;
            }
        }
        let mut limited_provider: Option<String> = None;
        // One classification per message, reused by retries.
        let previous = session.exchanges.last();
        let roles =
            crate::policy::claimed_roles(&registry.models, registry.uses_declared_preferences());
        let previous_user = previous.map(|e| e.user.as_str());
        let classification = if turn_model.is_some() {
            crate::jev::classify_local(&message, previous_user)
        } else {
            crate::jev::classify_turn(&message, previous_user, cfg, &roles)
        };
        // Stay on the model already doing this task: switching mid-task throws
        // away its session, its cache and what it has read.
        // A demanding follow-up ("this is harder than it looks, escalate") or a
        // mode change escalates instead.
        let demanding = crate::policy::preferred_role(
            classification.task,
            &message,
            options.mode,
            classification.tier,
        ) == "demanding";
        // Consumed every message, so a /mode before a new task doesn't linger.
        let rerouted = std::mem::take(&mut reroute);
        let continuing = cfg.inner.routing.sticky
            && classification.new_task == Some(false)
            && !rerouted
            && !demanding;
        let last_model = previous.map(|e| e.model.clone());
        // One /again is one retry, however many models the turn falls back through.
        let mut again_recorded = false;
        loop {
            let mut available = Registry {
                models: registry
                    .models
                    .iter()
                    .filter(|m| m.enabled && backend.available(m.runtime, cfg))
                    .filter(|m| !denied_models.contains(&m.id))
                    .filter(|m| !needs_login.iter().any(|p| p == m.provider.as_str()))
                    .filter(|m| {
                        !attempted.contains(&m.id)
                            && cooldowns.remaining(&m.id, m.provider.as_str()).is_none()
                    })
                    .cloned()
                    .collect(),
            };
            if available.models.is_empty() {
                writeln!(
                output,
                "No eligible model is available right now. The session remains open; use /models to see cooldowns and try again when capacity returns."
            )?;
                continue 'messages;
            }
            if turn_provider.is_none() && turn_model.is_none() {
                if let Some(provider) = &limited_provider {
                    if available
                        .models
                        .iter()
                        .any(|m| m.provider.as_str() != provider)
                    {
                        available.models.retain(|m| m.provider.as_str() != provider);
                    }
                }
            }
            let sticky = last_model.clone().filter(|m| {
                continuing
                    && turn_model.is_none()
                    && turn_provider.is_none()
                    && available.models.iter().any(|a| &a.id == m)
            });
            let (decision, mut why) = match router::route_summarized(
                classification.clone(),
                &message,
                cfg,
                &available,
                turn_mode,
                turn_provider,
                turn_model.clone().or(sticky.clone()),
            ) {
                Ok((mut decision, why)) => {
                    if let Some(model) = &sticky {
                        decision.overridden = false;
                        decision.reason = format!(
                            "{}; same task as the previous message, staying on {model}",
                            decision
                                .reason
                                .split("; explicit")
                                .next()
                                .unwrap_or_default()
                        );
                    }
                    (decision, why)
                }
                Err(error) => {
                    writeln!(
                        output,
                        "Routing failed: {error:#}. Use /model auto or /models."
                    )?;
                    continue 'messages;
                }
            };
            if options.dry_run {
                writeln!(output, "{}", serde_json::to_string_pretty(&decision)?)?;
                continue 'messages;
            }
            // The retry must not see the answer it replaces, nor resume the
            // session that wrote it. The answer leaves the log only with the
            // retry's own record (below), so a retry that can't run keeps it.
            let held = replaces.as_ref().and_then(|_| session.exchanges.pop());
            let live_session = if replaces.is_some() {
                None
            } else {
                provider_session.clone()
            };
            let budget = if decision.runtime == "ollama" {
                cfg.inner
                    .ollama
                    .as_ref()
                    .and_then(|o| {
                        o.models
                            .iter()
                            .find(|m| decision.model == format!("ollama/{}", m.name))
                    })
                    .map(|m| crate::session::budget_bytes(m.context_tokens))
                    .unwrap_or(CONTEXT_BYTES)
            } else {
                CONTEXT_BYTES
            };
            let resumable = cfg.inner.routing.resume_sessions
                && executor::ProviderSession::supported(&decision.runtime)
                && executor::can_resume(&decision.runtime, cfg);
            let mut turn_session = match &live_session {
                Some((model, id)) if resumable && model == &decision.model => {
                    executor::ProviderSession::resume(id)
                }
                _ => executor::ProviderSession::start(&decision.runtime),
            };
            // Another model taking over mid-task gets the outgoing one's handoff
            // note instead of a full transcript replay.
            let note = match &live_session {
                Some((model, id))
                    if cfg.inner.routing.handoff_summary
                        && !turn_session.resume
                        && model != &decision.model
                        && classification.new_task != Some(true) =>
                {
                    handoff_note(
                        model,
                        id,
                        &classification,
                        &message,
                        cfg,
                        registry,
                        options.mode,
                        backend,
                        output,
                    )?
                }
                _ => None,
            };
            let context = if turn_session.resume {
                Session::followup(&message).map(|p| (p, 0))
            } else if let Some(note) = &note {
                session.handoff(&message, note)
            } else {
                session.prompt(&message, budget)
            };
            if let Some(exchange) = held {
                session.exchanges.push(exchange);
            }
            let (prompt, omitted) = match context {
                Ok(context) => context,
                Err(error) => {
                    writeln!(output, "{error:#}")?;
                    continue 'messages;
                }
            };
            if omitted > 0 {
                writeln!(output, "Context window: {omitted} earlier turns omitted from this request; the saved log retains them.")?;
            }
            let kind = why.split(" · ").next().unwrap_or_default().to_string();
            if sticky.is_some() {
                why = format!("{kind} · same task as your last message");
            } else if let Some(Again::Stronger(beat)) = &again {
                why = format!("{kind} · /again: best available other than {beat}");
            } else if requested_model.is_none() && options.model.as_ref() == Some(&decision.model) {
                why = format!("{kind} · pinned with /model");
            }
            writeln!(
                output,
                "\n{} ({}) · {why}> ",
                decision.model, decision.provider
            )?;
            // Switching mid-task has a cost the user should see: the new model
            // starts without the old one's session and cache.
            match &last_model {
                Some(previous)
                    if previous != &decision.model
                        && again.is_none()
                        && classification.new_task != Some(true) =>
                {
                    if note.is_some() {
                        writeln!(output, "Switched from {previous}, which wrote {} a handoff note; its cached context isn't reused.", decision.model)?;
                    } else {
                        writeln!(output, "Switched from {previous}: the conversation is sent again, and its cached context isn't reused.")?;
                    }
                }
                _ => {}
            }
            if options.explain {
                writeln!(output, "{}", decision.reason)?;
            }
            output.flush()?;
            // Persist the input before launching a child; failures never become assistant context.
            let replaced = replaces.take();
            let route = crate::session::Route {
                why: why.clone(),
                handoff: note.is_some(),
                again: again.is_some() && !std::mem::replace(&mut again_recorded, true),
            };
            session.begin_routed(&message, &decision.model, replaced.is_some(), &route)?;
            if let Some(model) = replaced {
                writeln!(output, "{model}'s answer is kept in the log as replaced. Any files it changed stay changed.")?;
            }
            unanswered = Some(message.clone());
            attempted.push(decision.model.clone());
            // Track the session for `nectra usage` even when it won't be resumed.
            let keep = executor::ProviderSession::supported(&decision.runtime)
                .then_some(&mut turn_session);
            let response = if executor::native::enabled(cfg, &decision.runtime)? {
                backend.respond_session(&decision, &prompt, cfg, output, keep)
            } else {
                let activity = crate::activity::Activity::start(&decision.model);
                let mut progress = activity.writer(output);
                let _plain = crate::terminal::plain();
                backend.respond_session(&decision, &prompt, cfg, &mut progress, keep)
            };
            // A failed turn may have left the provider session half-written; start fresh.
            provider_session = None;
            match response {
                Ok(reply) if !reply.trim().is_empty() => {
                    successful_models.insert(decision.model.clone());
                    session.complete_in(&reply, turn_session.id.as_deref())?;
                    unanswered = None;
                    // Real chats only: a test run (no cooldown state) never reads real logs.
                    if cooldown_state.is_some() {
                        cost_line(session, output)?;
                    }
                    provider_session = turn_session
                        .id
                        .filter(|_| resumable)
                        .map(|id| (decision.model.clone(), id));
                }
                Ok(_) => {
                    session.fail_in("agent returned no answer", turn_session.id.as_deref())?;
                    writeln!(
                        output,
                        "Agent returned no answer. The session remains open."
                    )?;
                }
                Err(error) => {
                    session.fail_in(&format!("{error:#}"), turn_session.id.as_deref())?;
                    if let Some(unavailable) =
                        error.downcast_ref::<crate::access::ModelUnavailable>()
                    {
                        denied_models.insert(decision.model.clone());
                        successful_models.remove(&decision.model);
                        writeln!(output, "{}: {unavailable}. Excluded for this session; /model retry clears access observations.", decision.model)?;
                        if unavailable.safe_to_retry {
                            turn_model = None;
                            turn_provider = None;
                            writeln!(
                                output,
                                "Trying another eligible model; no work had started."
                            )?;
                            continue;
                        }
                        writeln!(output, "The native session cannot confirm that no work ran; this turn was not replayed. Future messages will use another eligible model.")?;
                        continue 'messages;
                    }
                    if let Some(auth) = error.downcast_ref::<executor::AuthenticationRequired>() {
                        needs_login.push(decision.provider.clone());
                        writeln!(output,"{} needs authentication. Run /login {} here, or nectra provider login {} in another terminal.",decision.provider,decision.provider,decision.provider)?;
                        if auth.safe_to_retry {
                            turn_model = None;
                            turn_provider = None;
                            continue;
                        }
                        continue 'messages;
                    }
                    if let Some(unavailable) = error.downcast_ref::<crate::ollama::Unavailable>() {
                        cooldowns.record(
                            &decision.model,
                            &decision.provider,
                            &crate::limits::RateLimited {
                                message: unavailable.message.clone(),
                                provider_wide: unavailable.provider_wide,
                                retry_after: std::time::Duration::from_secs(30),
                                safe_to_retry: unavailable.safe_to_retry,
                            },
                        );
                        remember(&cooldowns);
                        writeln!(
                            output,
                            "\n{} unavailable: {}. Cooling down for 30s.",
                            decision.model, unavailable.message
                        )?;
                        if unavailable.safe_to_retry {
                            turn_model = None;
                            turn_provider = None;
                            writeln!(
                                output,
                                "Trying another eligible model under your routing policy."
                            )?;
                            continue;
                        }
                        writeln!(output, "Partial output was not replayed or saved as an answer. The session remains open.")?;
                        continue 'messages;
                    }
                    if let Some(limit) = error.downcast_ref::<crate::limits::RateLimited>() {
                        cooldowns.record(&decision.model, &decision.provider, limit);
                        remember(&cooldowns);
                        limited_provider = Some(decision.provider.clone());
                        writeln!(
                            output,
                            "\n{} rate limited; {} cooldown for {}s.",
                            decision.model,
                            if limit.provider_wide {
                                "provider"
                            } else {
                                "model"
                            },
                            limit.retry_after.as_secs()
                        )?;
                        if limit.safe_to_retry {
                            turn_model = None;
                            turn_provider = None;
                            writeln!(output, "Trying an available alternative with the same conversation context; unavailable model/provider selections are relaxed for this turn.")?;
                            continue;
                        }
                        writeln!(output, "Work already started, so this message was not replayed. Future messages will avoid the limited models.")?;
                        continue 'messages;
                    }
                    writeln!(output, "\nTurn failed: {error:#}\nThe session remains open. You can choose another model and send a new message.")?;
                }
            }
            break;
        }
    }
    writeln!(output, "Session saved: {}", session.path.display())?;
    Ok(())
}

/// What `/again` asked of the next turn.
enum Again {
    /// Route afresh, excluding this model and judging on capability.
    Stronger(String),
    /// The last message got no answer; send it as it was.
    Same,
    /// This model, replacing the last answer when there is one.
    Model(String, Option<String>),
}

/// Resumes the outgoing model's session once to ask for a handoff note.
/// None (and a notice) when it can't answer, so the caller replays the transcript.
#[allow(clippy::too_many_arguments)]
fn handoff_note(
    model: &str,
    id: &str,
    classification: &crate::jev::Classification,
    message: &str,
    cfg: &Config,
    registry: &Registry,
    mode: Mode,
    backend: &mut dyn Backend,
    output: &mut dyn Write,
) -> Result<Option<String>> {
    let Ok(outgoing) = router::route_with(
        classification.clone(),
        message,
        cfg,
        registry,
        mode,
        None,
        Some(model.to_string()),
    ) else {
        return Ok(None);
    };
    writeln!(output, "Asking {model} for a handoff note.")?;
    let mut session = executor::ProviderSession::resume(id);
    match backend.handoff(&outgoing, Session::HANDOFF_REQUEST, cfg, &mut session) {
        Ok(note) if !note.trim().is_empty() => Ok(Some(note)),
        _ => {
            writeln!(output, "No handoff note; sending the conversation instead.")?;
            Ok(None)
        }
    }
}

/// The chat's running API-equivalent cost, beside the same tokens at the baseline's prices.
fn cost_line(session: &Session, output: &mut dyn Write) -> Result<()> {
    let Ok(logs) = crate::usage::Logs::default_paths() else {
        return Ok(());
    };
    let base = crate::usage::BASELINE;
    // Best effort: a provider log that can't be read never fails the turn.
    if let Ok(Some((cost, at_base))) =
        crate::usage::chat_cost(&logs, &session.provider_sessions, base)
    {
        let usd = crate::usage::usd;
        let compare = if (at_base - cost).abs() > 0.0005 {
            format!("; the same tokens at {base} prices: {}", usd(at_base))
        } else {
            String::new()
        };
        writeln!(output, "This chat: {} at API prices{compare}.", usd(cost))?;
    }
    Ok(())
}

/// Deliberately narrow leading directives: mentioning a model in a task is not a pin.
fn requested_model(message: &str, registry: &Registry) -> Option<String> {
    let lower = message.trim_start().to_ascii_lowercase();
    let lower = lower.strip_prefix("please ").unwrap_or(&lower);
    let rest = ["use ", "switch to ", "ask "]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix))?;
    registry.models.iter().find_map(|model| {
        let id = model.id.to_ascii_lowercase();
        let tail = rest.strip_prefix(&id)?;
        (tail.is_empty()
            || tail.starts_with(char::is_whitespace)
            || tail.starts_with(':')
            || tail.starts_with(','))
        .then(|| model.id.clone())
    })
}
