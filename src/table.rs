//! The task kind x difficulty tier table: which model, in what order.
//!
//! Laya says what the work is (kind) and how hard it looks (tier, with a
//! confidence); the registry's `[[table]]` entries say which model takes it.
//! Each entry is an ordered ladder, cheapest acceptable model first, and each
//! rung carries the measurement behind it. Without an entry the router keeps
//! its role-and-score path (docs/routing.md).
use crate::{
    cli::Mode,
    config::{RoutingConfig, TierHedge},
    jev::Tier,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Every task kind a classifier can name (`classifier::TaskType`).
pub const KINDS: [&str; 10] = [
    "code_generation",
    "debugging",
    "refactoring",
    "code_review",
    "architecture",
    "research",
    "writing",
    "planning",
    "data_analysis",
    "general",
];

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Simple => "simple",
            Tier::Normal => "normal",
            Tier::Demanding => "demanding",
        }
    }
    pub fn parse(s: &str) -> Option<Tier> {
        [Tier::Simple, Tier::Normal, Tier::Demanding]
            .into_iter()
            .find(|t| t.as_str() == s)
    }
}

/// One model on a ladder and the evidence for putting it there.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rung {
    pub model: String,
    /// Graded trials behind the pass rate.
    pub n: u32,
    pub pass_rate: f64,
    /// Confidence interval on the pass rate: `[low, high]`.
    pub ci: [f64; 2],
    /// The calibration run the numbers come from.
    pub run: String,
    /// Mean measured cost of one run on this model, when the calibration
    /// reports it; `top2` compares picks by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// What the numbers were measured on; see [`Basis`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<Basis>,
}

/// The identity the measurement is bound to. A rung measured on another
/// effort, harness or task set than the registry's model is stale.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    /// `complete`, `partial` or `quarantined`; only `complete` is imported.
    pub status: String,
    /// The model's reasoning effort during the run ("" = the default).
    pub effort: String,
    /// The harness and check version that graded the runs.
    pub harness: String,
    /// Hash of the task set.
    pub task_set: String,
    /// Date measured (YYYY-MM-DD).
    pub measured: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TableEntry {
    pub kind: String,
    pub tier: String,
    /// Cheapest acceptable model first.
    pub ladder: Vec<Rung>,
}

impl TableEntry {
    pub fn validate(&self) -> Result<()> {
        let at = format!("table {} x {}", self.kind, self.tier);
        if !KINDS.contains(&self.kind.as_str()) {
            bail!("{at}: unknown kind (known: {})", KINDS.join(", "));
        }
        if Tier::parse(&self.tier).is_none() {
            bail!("{at}: tier must be simple, normal or demanding");
        }
        if self.ladder.is_empty() {
            bail!("{at}: empty ladder");
        }
        let mut seen = std::collections::HashSet::new();
        for r in &self.ladder {
            if !seen.insert(&r.model) {
                bail!("{at}: {} appears twice on the ladder", r.model);
            }
            let [lo, hi] = r.ci;
            let ok = [r.pass_rate, lo, hi]
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v));
            if !ok || lo > hi || r.pass_rate < lo || r.pass_rate > hi {
                bail!(
                    "{at}: {}: pass_rate and ci must be within 0..1 with ci low <= pass_rate <= ci high",
                    r.model
                );
            }
            if r.n == 0 || r.run.trim().is_empty() {
                bail!("{at}: {}: evidence needs n > 0 and a run id", r.model);
            }
            if let Some(c) = r.cost_usd {
                if !c.is_finite() || c < 0.0 {
                    bail!("{at}: {}: cost_usd must be a finite number >= 0", r.model);
                }
            }
            if let Some(b) = &r.basis {
                if b.status != "complete" {
                    bail!(
                        "{at}: {}: evidence status is {}; only complete evidence is imported",
                        r.model,
                        b.status
                    );
                }
            }
        }
        Ok(())
    }
}

/// Check a whole table: entries valid, one per kind x tier, models known.
pub fn validate(table: &[TableEntry], known: impl Fn(&str) -> bool) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for e in table {
        e.validate()?;
        if !seen.insert((&e.kind, &e.tier)) {
            bail!("table {} x {} appears twice", e.kind, e.tier);
        }
        if let Some(r) = e.ladder.iter().find(|r| !known(&r.model)) {
            bail!(
                "table {} x {}: {} is not in the model registry",
                e.kind,
                e.tier,
                r.model
            );
        }
    }
    Ok(())
}

/// The calibration output `nectra registry table import` reads.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    /// Default run id for rungs that carry none.
    #[serde(default)]
    run: Option<String>,
    entries: Vec<CalibrationEntry>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationEntry {
    kind: String,
    tier: String,
    ladder: Vec<CalibrationRung>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationRung {
    model: String,
    n: u32,
    pass_rate: f64,
    ci: [f64; 2],
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    cost_usd: Option<f64>,
    #[serde(default)]
    basis: Option<Basis>,
}

pub fn parse_calibration(json: &str) -> Result<Vec<TableEntry>> {
    let c: Calibration = serde_json::from_str(json).context("parsing calibration JSON")?;
    let table: Vec<TableEntry> = c
        .entries
        .into_iter()
        .map(|e| {
            let ladder = e
                .ladder
                .into_iter()
                .map(|r| {
                    Ok(Rung {
                        run: r
                            .run
                            .or_else(|| c.run.clone())
                            .with_context(|| format!("{}: no run id", r.model))?,
                        model: r.model,
                        n: r.n,
                        pass_rate: r.pass_rate,
                        ci: r.ci,
                        cost_usd: r.cost_usd,
                        basis: r.basis,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(TableEntry {
                kind: e.kind,
                tier: e.tier,
                ladder,
            })
        })
        .collect::<Result<_>>()?;
    validate(&table, |_| true)?;
    Ok(table)
}

/// Drop rungs naming a model the registry no longer has, and cells left with
/// no rung (those route the old way). Returns one line per dropped rung.
pub fn prune(table: &mut Vec<TableEntry>, known: impl Fn(&str) -> bool) -> Vec<String> {
    let mut dropped = vec![];
    for e in table.iter_mut() {
        let (kind, tier) = (e.kind.clone(), e.tier.clone());
        e.ladder.retain(|r| {
            let keep = known(&r.model);
            if !keep {
                dropped.push(format!("{kind} x {tier}: {}", r.model));
            }
            keep
        });
    }
    table.retain(|e| !e.ladder.is_empty());
    dropped
}

/// Rungs measured on a different effort than the registry's model now runs
/// at are stale: drop them (`drop`) or just list them. `effort_of` gives a
/// model's current effort.
pub fn prune_stale(
    table: &mut Vec<TableEntry>,
    effort_of: impl Fn(&str) -> Option<String>,
    drop: bool,
) -> Vec<String> {
    let mut stale = vec![];
    for e in table.iter_mut() {
        let (kind, tier) = (e.kind.clone(), e.tier.clone());
        e.ladder.retain(|r| {
            let now = effort_of(&r.model);
            let bad = matches!((&r.basis, &now), (Some(b), Some(n)) if &b.effort != n);
            if bad {
                stale.push(format!(
                    "{kind} x {tier}: {} (measured at effort {:?}, now {:?})",
                    r.model,
                    r.basis.as_ref().map(|b| b.effort.as_str()).unwrap_or(""),
                    now.as_deref().unwrap_or("")
                ));
            }
            !(bad && drop)
        });
    }
    table.retain(|e| !e.ladder.is_empty());
    stale
}

/// Warn about dropped rungs, once per process: `missing` name a model the
/// registry no longer has, `stale` were measured at another effort.
pub fn warn_dropped(missing: &[String], stale: &[String]) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if missing.is_empty() && stale.is_empty() {
        return;
    }
    ONCE.call_once(|| {
        if !missing.is_empty() {
            eprintln!(
                "nectra: the table names models that are not in the model registry; ignoring those rungs ({}). \
                 Re-run `nectra registry table import` to repair the table.",
                missing.join(", ")
            );
        }
        if !stale.is_empty() {
            eprintln!(
                "nectra: the table has evidence measured at a different effort than the model now runs at; ignoring those rungs until they are re-measured ({}).",
                stale.join(", ")
            );
        }
    });
}

const BEGIN: &str =
    "# --- nectra registry table import: begin (generated; replaced by the next import) ---";
const END: &str = "# --- nectra registry table import: end ---";

/// The `[[table]]` TOML for `table`, fenced so a later import replaces it.
pub fn render(table: &[TableEntry]) -> Result<String> {
    #[derive(Serialize)]
    struct File<'a> {
        table: &'a [TableEntry],
    }
    Ok(format!(
        "{BEGIN}\n{}{END}\n",
        toml::to_string(&File { table })?
    ))
}

/// `registry_text` with its previously imported table (if any) replaced by
/// `table`. Hand-written `[[table]]` entries outside the fence are kept, and
/// loading refuses a duplicate kind x tier.
pub fn splice(registry_text: &str, table: &[TableEntry]) -> Result<String> {
    let crlf = registry_text.contains("\r\n");
    let block = render(table)?;
    let block = if crlf {
        block.replace('\n', "\r\n")
    } else {
        block
    };
    match (registry_text.find(BEGIN), registry_text.find(END)) {
        (Some(a), Some(b)) if a < b => {
            let end = b + END.len();
            let rest = &registry_text[end..];
            let end = end
                + if rest.starts_with("\r\n") {
                    2
                } else {
                    usize::from(rest.starts_with('\n'))
                };
            Ok(format!(
                "{}{block}{}",
                &registry_text[..a],
                &registry_text[end..]
            ))
        }
        (None, None) => {
            let nl = if crlf { "\r\n" } else { "\n" };
            let mut text = registry_text.to_string();
            if !text.ends_with('\n') {
                text.push_str(nl);
            }
            Ok(format!("{text}{nl}{block}"))
        }
        _ => {
            bail!("the registry's imported-table markers are damaged; remove both lines and retry")
        }
    }
}

/// What the router knows when it asks for a pick.
pub struct Inputs<'a> {
    pub kind: &'a str,
    pub tier: Tier,
    /// The classifier's confidence in the task kind; None for keywords.
    pub confidence: Option<f64>,
    /// Distance of Laya's difficulty from the nearest tier boundary; None for keywords.
    pub margin: Option<f64>,
    /// The tier on the other side of that boundary.
    pub neighbour: Option<Tier>,
}

/// Where the router's pick came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    pub model: String,
    pub kind: String,
    pub tier: String,
    /// Position on the ladder, 0 = cheapest.
    pub rung: usize,
    pub ladder_len: usize,
    pub hedged: bool,
    pub evidence: String,
}

/// An escalation that cannot go on: past the cap, or no rung above.
#[derive(Debug)]
pub struct LadderLimit(pub String);
impl std::fmt::Display for LadderLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for LadderLimit {}

/// How a retry steers the pick.
#[derive(Debug, Clone, Default)]
pub struct Steer {
    /// Step above this model on the ladder: the one that just failed.
    pub above: Option<String>,
    /// Escalations the task has already taken, counting this one.
    pub escalations: u32,
    /// Do not climb: the ladder is exhausted, so route normally but never
    /// back onto `above`.
    pub stay: bool,
    /// The cell (kind, tier) the task's first pick came from. Every retry
    /// climbs this one ladder, whatever the prompt classifies as later.
    pub cell: Option<(String, String)>,
    /// Every model already tried for this task: never picked again.
    pub attempted: Vec<String>,
}

/// Ladder indexes by measured quality, weakest first; equal pass rates keep
/// the cheaper (earlier) rung first.
fn quality_order(e: &TableEntry) -> Vec<usize> {
    let mut o: Vec<usize> = (0..e.ladder.len()).collect();
    o.sort_by(|&a, &b| {
        e.ladder[a]
            .pass_rate
            .total_cmp(&e.ladder[b].pass_rate)
            .then(a.cmp(&b))
    });
    o
}

fn find<'a>(table: &'a [TableEntry], kind: &str, tier: Tier) -> Option<&'a TableEntry> {
    table
        .iter()
        .find(|e| e.kind == kind && e.tier == tier.as_str())
}

fn evidence(r: &Rung) -> String {
    format!(
        "pass {:.0}% [{:.0}-{:.0}%] n={} run {}",
        r.pass_rate * 100.0,
        r.ci[0] * 100.0,
        r.ci[1] * 100.0,
        r.n,
        r.run
    )
}

/// The table's pick, `None` when no entry applies (the router then takes its
/// own path). `usable` says which models are eligible right now; an unusable
/// rung is skipped. An escalation past the cap or past the top of the ladder
/// is an error rather than a silent fallback to something unmeasured.
pub fn select(
    table: &[TableEntry],
    inp: &Inputs,
    mode: Mode,
    cfg: &RoutingConfig,
    usable: &dyn Fn(&str) -> bool,
    steer: &Steer,
) -> Result<Option<Selection>> {
    // `max_escalations = 0` turns escalation off: retries route as first tries.
    let off = Steer::default();
    let steer = if cfg.max_escalations == 0 {
        &off
    } else {
        steer
    };
    let pinned = steer
        .cell
        .as_ref()
        .and_then(|(k, t)| table.iter().find(|e| &e.kind == k && &e.tier == t));
    let Some(entry) = pinned.or_else(|| find(table, inp.kind, inp.tier)) else {
        return Ok(None);
    };
    let tried = |m: &str| steer.attempted.iter().any(|a| a == m);
    if steer.escalations > cfg.max_escalations {
        return Err(LadderLimit(format!(
            "escalation cap reached ({} step{})",
            cfg.max_escalations,
            if cfg.max_escalations == 1 { "" } else { "s" }
        ))
        .into());
    }
    let pick = |e: &TableEntry, i: usize, hedged: bool| {
        let r = &e.ladder[i];
        Selection {
            model: r.model.clone(),
            kind: e.kind.clone(),
            tier: e.tier.clone(),
            rung: i,
            ladder_len: e.ladder.len(),
            hedged,
            evidence: evidence(r),
        }
    };
    // A retry climbs from the model that failed, whatever the mode or the
    // doubt about the tier: one rung above it on the ladder it was picked
    // from (the hedge may have taken it from a neighbouring tier). It never
    // stands still or steps down.
    if let Some(above) = steer.above.as_ref().filter(|_| !steer.stay) {
        // One immutable ladder per task: the cell of the first pick when the
        // caller knows it, else the one ladder (preferring the tier's own)
        // that still has room above the failed model.
        let holding: Vec<&TableEntry> = if steer.cell.is_some() && pinned.is_some() {
            vec![entry]
        } else {
            std::iter::once(entry)
                .chain(
                    table
                        .iter()
                        .filter(|e| e.kind == inp.kind && !std::ptr::eq(*e, entry)),
                )
                .filter(|e| e.ladder.iter().any(|r| &r.model == above))
                .collect()
        };
        // Retries climb by measured quality (pass rate, ties to the cheaper
        // rung), not by cost: cost order is only the initial-choice order.
        let order = |e: &TableEntry| quality_order(e);
        let from = |e: &TableEntry| {
            order(e)
                .iter()
                .rposition(|&i| e.ladder[i].model == *above || tried(&e.ladder[i].model))
        };
        let next_of = |e: &TableEntry| {
            let o = order(e);
            from(e).and_then(|p| {
                o[p + 1..]
                    .iter()
                    .copied()
                    .find(|&i| usable(&e.ladder[i].model) && !tried(&e.ladder[i].model))
            })
        };
        let can_climb = |e: &&TableEntry| next_of(e).is_some();
        let climb = holding
            .iter()
            .find(|e| can_climb(e))
            .or(holding.first())
            .copied()
            .filter(|e| from(e).is_some());
        if let Some(e) = climb {
            if let Some(next) = next_of(e) {
                return Ok(Some(pick(e, next, false)));
            }
            let p = from(e).unwrap_or(0);
            let why = if p + 1 < e.ladder.len() {
                format!(
                    "the rungs above {above} on the {} x {} ladder are unavailable or already tried",
                    e.kind, e.tier
                )
            } else {
                format!("{above} is the top of the {} x {} ladder", e.kind, e.tier)
            };
            return Err(LadderLimit(why).into());
        }
    }
    // A failed model that is on no ladder is not tried again either.
    let usable = |m: &str| usable(m) && steer.above.as_deref() != Some(m) && !tried(m);
    let ok: Vec<usize> = (0..entry.ladder.len())
        .filter(|&i| usable(&entry.ladder[i].model))
        .collect();
    let Some(&first) = ok.first() else {
        return Ok(None);
    };
    // The cheapest rung for --mode fast, the strongest for best, else the
    // cheapest acceptable one, one higher when the tier is a close call.
    let mut idx = match mode {
        Mode::Fast => first,
        // The best measured quality among the usable rungs (ties to the cheaper).
        Mode::Best => ok
            .iter()
            .copied()
            .max_by(|&a, &b| {
                entry.ladder[a]
                    .pass_rate
                    .total_cmp(&entry.ladder[b].pass_rate)
                    .then(b.cmp(&a))
            })
            .unwrap_or(first),
        Mode::Balanced => first,
    };
    let mut hedged = false;
    let mut chosen: Option<(&TableEntry, usize)> = None;
    let uncertain = inp
        .confidence
        .is_some_and(|c| c < cfg.hedge_confidence_below)
        || inp.margin.is_some_and(|m| m < cfg.hedge_margin_below);
    if mode == Mode::Balanced && uncertain {
        match cfg.tier_hedge {
            TierHedge::Off => {}
            TierHedge::Rung => {
                if let Some(&next) = ok.iter().find(|&&i| i > idx) {
                    idx = next;
                    hedged = true;
                }
            }
            TierHedge::Top2 => {
                if let Some(other) = inp
                    .neighbour
                    .filter(|n| *n > inp.tier)
                    .and_then(|n| find(table, inp.kind, n))
                {
                    if let Some(j) =
                        (0..other.ladder.len()).find(|&j| usable(&other.ladder[j].model))
                    {
                        // The dearer of the two picks (by measured cost); with
                        // no costs on record, the higher tier's pick.
                        let own = entry.ladder[idx].cost_usd;
                        let theirs = other.ladder[j].cost_usd;
                        if !matches!((own, theirs), (Some(a), Some(b)) if a > b) {
                            chosen = Some((other, j));
                            hedged = true;
                        }
                    }
                }
            }
        }
    }
    let (e, i) = chosen.unwrap_or((entry, idx));
    Ok(Some(pick(e, i, hedged)))
}

/// `nectra registry table import|show`.
pub fn command(
    cfg: &crate::config::Config,
    registry: &crate::models::Registry,
    command: &crate::cli::TableCommands,
) -> Result<i32> {
    use crate::cli::TableCommands;
    match command {
        TableCommands::Show { json } => {
            if *json {
                println!("{}", serde_json::to_string_pretty(&registry.table)?);
            } else if registry.table.is_empty() {
                println!(
                    "No table in {}: routing uses roles and scores.",
                    cfg.registry_path().display()
                );
            }
            if !*json && !registry.table.is_empty() {
                let unmeasured: Vec<&str> = KINDS
                    .iter()
                    .copied()
                    .filter(|k| !registry.table.iter().any(|e| e.kind == *k))
                    .collect();
                if !unmeasured.is_empty() {
                    println!(
                        "UNMEASURED (no cells; routed by the heuristic): {}",
                        unmeasured.join(", ")
                    );
                }
            }
            for e in registry.table.iter().filter(|_| !*json) {
                println!("{} x {}", e.kind, e.tier);
                for (i, r) in e.ladder.iter().enumerate() {
                    println!("  {}. {:<20} {}", i + 1, r.model, evidence(r));
                }
            }
            Ok(0)
        }
        TableCommands::Import { json, dry_run } => {
            let text = std::fs::read_to_string(json)
                .with_context(|| format!("reading {}", json.display()))?;
            let table = parse_calibration(&text)?;
            validate(&table, |id| registry.models.iter().any(|m| m.id == id))?;
            let mut copy = table.clone();
            let stale = prune_stale(
                &mut copy,
                |id| {
                    registry
                        .models
                        .iter()
                        .find(|m| m.id == id)
                        .map(|m| m.effort.clone())
                },
                false,
            );
            if !stale.is_empty() {
                eprintln!(
                    "nectra: warning: evidence was measured on a different effort than the registry's model; the router will ignore these rungs until they are re-measured: {}",
                    stale.join(", ")
                );
            }
            if cfg.embedded {
                bail!("no registry file to write: copy config/models.toml beside your config.toml first");
            }
            let path = cfg.registry_path();
            let current = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let next = splice(&current, &table)?;
            // The whole file must still load as a registry: a duplicate with a
            // hand-written entry would otherwise lock every command out.
            crate::models::Registry::from_text(cfg, &next)
                .context("the registry would not load with this table; nothing written")?;
            if *dry_run {
                println!(
                    "{} entries, nothing written. {} would hold:\n{}",
                    table.len(),
                    path.display(),
                    render(&table)?
                );
                return Ok(0);
            }
            // Write beside the real file (through a symlink), with its
            // permissions, synced, and leave nothing behind on failure.
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            let tmp = path.with_extension("toml.new");
            let written = (|| -> Result<()> {
                use std::io::Write;
                let mut f = std::fs::File::create(&tmp)
                    .with_context(|| format!("writing {}", tmp.display()))?;
                if let Ok(meta) = std::fs::metadata(&path) {
                    let _ = f.set_permissions(meta.permissions());
                }
                f.write_all(next.as_bytes())?;
                f.sync_all()?;
                std::fs::rename(&tmp, &path)
                    .with_context(|| format!("replacing {}", path.display()))
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            println!(
                "Imported {} table entries into {}.",
                table.len(),
                path.display()
            );
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rung(model: &str, pass: f64) -> Rung {
        Rung {
            model: model.into(),
            n: 40,
            pass_rate: pass,
            ci: [pass - 0.1, (pass + 0.05).min(1.0)],
            run: "rh-v2-test".into(),
            cost_usd: None,
            basis: None,
        }
    }
    fn entry(kind: &str, tier: &str, models: &[&str]) -> TableEntry {
        TableEntry {
            kind: kind.into(),
            tier: tier.into(),
            ladder: models
                .iter()
                .enumerate()
                .map(|(i, m)| rung(m, 0.7 + 0.1 * i as f64))
                .collect(),
        }
    }
    fn table() -> Vec<TableEntry> {
        vec![
            entry("debugging", "simple", &["haiku", "sonnet"]),
            entry("debugging", "normal", &["sonnet", "gpt", "opus"]),
            entry("debugging", "demanding", &["gpt", "opus"]),
        ]
    }
    fn inputs(tier: Tier) -> Inputs<'static> {
        Inputs {
            kind: "debugging",
            tier,
            confidence: Some(0.95),
            margin: Some(0.2),
            neighbour: None,
        }
    }
    fn pick(t: &[TableEntry], i: &Inputs, mode: Mode, cfg: &RoutingConfig, s: &Steer) -> String {
        select(t, i, mode, cfg, &|_| true, s)
            .unwrap()
            .unwrap()
            .model
    }
    fn cfg() -> RoutingConfig {
        RoutingConfig::default()
    }

    #[test]
    fn the_cheapest_rung_takes_a_confident_pick() {
        let got = pick(
            &table(),
            &inputs(Tier::Normal),
            Mode::Balanced,
            &cfg(),
            &Steer::default(),
        );
        assert_eq!(got, "sonnet");
    }

    #[test]
    fn no_entry_means_no_pick() {
        let mut i = inputs(Tier::Normal);
        i.kind = "writing";
        let got = select(
            &table(),
            &i,
            Mode::Balanced,
            &cfg(),
            &|_| true,
            &Steer::default(),
        );
        assert_eq!(got.unwrap(), None);
    }

    #[test]
    fn low_confidence_starts_one_rung_higher() {
        let mut i = inputs(Tier::Normal);
        i.confidence = Some(0.4);
        let s = select(
            &table(),
            &i,
            Mode::Balanced,
            &cfg(),
            &|_| true,
            &Steer::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!((s.model.as_str(), s.rung, s.hedged), ("gpt", 1, true));
    }

    #[test]
    fn a_thin_tier_margin_hedges_too() {
        let mut i = inputs(Tier::Normal);
        i.margin = Some(0.01);
        assert_eq!(
            pick(&table(), &i, Mode::Balanced, &cfg(), &Steer::default()),
            "gpt"
        );
    }

    #[test]
    fn hedge_off_trusts_the_tier() {
        let mut i = inputs(Tier::Normal);
        i.confidence = Some(0.1);
        let mut c = cfg();
        c.tier_hedge = TierHedge::Off;
        assert_eq!(
            pick(&table(), &i, Mode::Balanced, &c, &Steer::default()),
            "sonnet"
        );
    }

    #[test]
    fn the_hedge_stops_at_the_top_of_the_ladder() {
        let mut i = inputs(Tier::Demanding);
        i.confidence = Some(0.1);
        i.neighbour = Some(Tier::Normal);
        // gpt -> opus once, never past it.
        assert_eq!(
            pick(&table(), &i, Mode::Balanced, &cfg(), &Steer::default()),
            "opus"
        );
        let one = vec![entry("debugging", "demanding", &["opus"])];
        assert_eq!(
            pick(&one, &i, Mode::Balanced, &cfg(), &Steer::default()),
            "opus"
        );
    }

    #[test]
    fn top2_takes_the_dearer_of_two_tiers_picks() {
        let mut i = inputs(Tier::Simple);
        i.margin = Some(0.01);
        i.neighbour = Some(Tier::Normal);
        let mut c = cfg();
        c.tier_hedge = TierHedge::Top2;
        let s = select(
            &table(),
            &i,
            Mode::Balanced,
            &c,
            &|_| true,
            &Steer::default(),
        )
        .unwrap()
        .unwrap();
        // simple would be haiku; normal's cheapest is sonnet.
        assert_eq!((s.model.as_str(), s.tier.as_str()), ("sonnet", "normal"));
        // A lower neighbour never lowers the pick.
        let mut j = inputs(Tier::Demanding);
        j.margin = Some(0.01);
        j.neighbour = Some(Tier::Normal);
        assert_eq!(
            pick(&table(), &j, Mode::Balanced, &c, &Steer::default()),
            "gpt"
        );
    }

    #[test]
    fn fast_takes_the_cheapest_rung_whatever_the_doubt() {
        let mut i = inputs(Tier::Normal);
        i.confidence = Some(0.1);
        assert_eq!(
            pick(&table(), &i, Mode::Fast, &cfg(), &Steer::default()),
            "sonnet"
        );
    }

    #[test]
    fn unusable_rungs_are_skipped() {
        let s = select(
            &table(),
            &inputs(Tier::Normal),
            Mode::Balanced,
            &cfg(),
            &|m| m != "sonnet",
            &Steer::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "gpt");
    }

    #[test]
    fn a_failure_steps_one_rung_above_the_failed_model() {
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        assert_eq!(
            pick(
                &table(),
                &inputs(Tier::Normal),
                Mode::Balanced,
                &cfg(),
                &steer
            ),
            "gpt"
        );
        let steer = Steer {
            above: Some("gpt".into()),
            escalations: 2,
            ..Default::default()
        };
        assert_eq!(
            pick(
                &table(),
                &inputs(Tier::Normal),
                Mode::Balanced,
                &cfg(),
                &steer
            ),
            "opus"
        );
    }

    #[test]
    fn escalation_never_steps_past_the_ladder_or_the_cap() {
        let top = Steer {
            above: Some("opus".into()),
            escalations: 1,
            ..Default::default()
        };
        let err = select(
            &table(),
            &inputs(Tier::Normal),
            Mode::Balanced,
            &cfg(),
            &|_| true,
            &top,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("top of"), "{err}");
        let mut c = cfg();
        c.max_escalations = 1;
        let capped = Steer {
            above: Some("sonnet".into()),
            escalations: 2,
            ..Default::default()
        };
        let err = select(
            &table(),
            &inputs(Tier::Normal),
            Mode::Balanced,
            &c,
            &|_| true,
            &capped,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("cap"), "{err}");
    }

    #[test]
    fn a_hedge_already_above_the_failure_is_not_undone() {
        let mut i = inputs(Tier::Normal);
        i.confidence = Some(0.1);
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        // Hedged start is gpt; one above sonnet is also gpt.
        assert_eq!(pick(&table(), &i, Mode::Balanced, &cfg(), &steer), "gpt");
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        let mut c = cfg();
        c.tier_hedge = TierHedge::Off;
        assert_eq!(pick(&table(), &i, Mode::Balanced, &c, &steer), "gpt");
    }

    #[test]
    fn validation_rejects_bad_entries() {
        let known = |m: &str| ["haiku", "sonnet", "gpt", "opus"].contains(&m);
        assert!(validate(&table(), known).is_ok());
        let mut bad = table();
        bad[0].kind = "debuging".into();
        assert!(validate(&bad, known)
            .unwrap_err()
            .to_string()
            .contains("unknown kind"));
        let mut bad = table();
        bad[0].ladder[0].ci = [0.9, 0.95];
        assert!(validate(&bad, known)
            .unwrap_err()
            .to_string()
            .contains("ci"));
        let mut bad = table();
        bad.push(bad[0].clone());
        assert!(validate(&bad, known)
            .unwrap_err()
            .to_string()
            .contains("twice"));
        let mut bad = table();
        bad[0].ladder[0].model = "ghost".into();
        assert!(validate(&bad, known)
            .unwrap_err()
            .to_string()
            .contains("ghost"));
        let mut bad = table();
        bad[1].ladder.clear();
        assert!(validate(&bad, known)
            .unwrap_err()
            .to_string()
            .contains("empty"));
    }

    #[test]
    fn calibration_json_imports_with_a_default_run() {
        let json = r#"{"run":"rh-v2-0412","entries":[{"kind":"debugging","tier":"normal","ladder":[
            {"model":"sonnet","n":40,"pass_rate":0.8,"ci":[0.66,0.9]},
            {"model":"opus","n":40,"pass_rate":0.95,"ci":[0.84,0.99],"run":"rh-v2-other"}]}]}"#;
        let t = parse_calibration(json).unwrap();
        assert_eq!(t[0].ladder[0].run, "rh-v2-0412");
        assert_eq!(t[0].ladder[1].run, "rh-v2-other");
        assert!(parse_calibration(r#"{"entries":[{"kind":"debugging","tier":"normal","ladder":[{"model":"a","n":1,"pass_rate":0.5,"ci":[0.1,0.9]}]}]}"#).is_err());
    }

    #[test]
    fn splice_replaces_only_the_fenced_block_and_roundtrips() {
        let base = "[[model]]\nid = \"x\"\n";
        let once = splice(base, &table()).unwrap();
        let parsed: toml::Value = toml::from_str(&once).unwrap();
        assert_eq!(parsed["table"].as_array().unwrap().len(), 3);
        let t2 = vec![entry("writing", "normal", &["sonnet"])];
        let twice = splice(&once, &t2).unwrap();
        let parsed: toml::Value = toml::from_str(&twice).unwrap();
        assert_eq!(parsed["table"].as_array().unwrap().len(), 1);
        assert!(twice.starts_with(base));
        assert_eq!(twice.matches("begin").count(), 1);
        assert!(splice("a\n# --- nectra registry table import: begin (generated; replaced by the next import) ---\n", &t2).is_err());
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    fn rung(model: &str) -> Rung {
        Rung {
            model: model.into(),
            n: 40,
            pass_rate: 0.8,
            ci: [0.7, 0.9],
            run: "r".into(),
            cost_usd: None,
            basis: None,
        }
    }
    fn entry(tier: &str, models: &[&str]) -> TableEntry {
        TableEntry {
            kind: "debugging".into(),
            tier: tier.into(),
            ladder: models.iter().map(|m| rung(m)).collect(),
        }
    }
    fn inp(tier: Tier) -> Inputs<'static> {
        Inputs {
            kind: "debugging",
            tier,
            confidence: Some(0.9),
            margin: Some(0.5),
            neighbour: None,
        }
    }
    #[test]
    fn a_top2_pick_that_failed_climbs_its_own_ladder() {
        let t = vec![
            entry("simple", &["haiku", "sonnet"]),
            entry("normal", &["gpt", "opus"]),
        ];
        let steer = Steer {
            above: Some("gpt".into()),
            escalations: 1,
            ..Default::default()
        };
        let s = select(
            &t,
            &inp(Tier::Simple),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "opus");
    }
    #[test]
    fn a_failed_model_off_every_ladder_is_not_picked_again() {
        let t = vec![entry("normal", &["sonnet", "opus"])];
        let steer = Steer {
            above: Some("sonnet-x".into()),
            escalations: 1,
            ..Default::default()
        };
        let s = select(
            &t,
            &inp(Tier::Normal),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "sonnet");
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        let s = select(
            &t,
            &inp(Tier::Normal),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "opus");
    }
    #[test]
    fn unavailable_rungs_are_not_called_the_top_and_cap_zero_disables() {
        let t = vec![entry("normal", &["sonnet", "opus"])];
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        let err = select(
            &t,
            &inp(Tier::Normal),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|m| m != "opus",
            &steer,
        )
        .unwrap_err();
        assert!(
            err.downcast_ref::<LadderLimit>().is_some() && err.to_string().contains("unavailable"),
            "{err}"
        );
        let off = RoutingConfig {
            max_escalations: 0,
            ..RoutingConfig::default()
        };
        let s = select(
            &t,
            &inp(Tier::Normal),
            Mode::Balanced,
            &off,
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            s.model, "sonnet",
            "escalation off: a retry routes as a first try"
        );
    }
    #[test]
    fn splice_keeps_crlf_files_consistent() {
        let t = vec![entry("normal", &["sonnet"])];
        let base = "[[model]]\r\nid = \"x\"\r\n";
        let once = splice(base, &t).unwrap();
        let twice = splice(&once, &t).unwrap();
        assert_eq!(once, twice);
        assert!(!twice.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn overlapping_ladders_climb_the_one_with_room() {
        // A Top2 pick of sonnet from the normal ladder: sonnet also tops the
        // simple ladder, but gpt and opus are above it on the normal one.
        let t = vec![
            entry("simple", &["haiku", "sonnet"]),
            entry("normal", &["sonnet", "gpt", "opus"]),
        ];
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
            ..Default::default()
        };
        let s = select(
            &t,
            &inp(Tier::Simple),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "gpt");
    }
    #[test]
    fn stay_never_returns_the_failed_model_and_never_climbs() {
        let t = vec![entry("simple", &["haiku", "sonnet"])];
        let steer = Steer {
            above: Some("haiku".into()),
            escalations: 0,
            stay: true,
            ..Default::default()
        };
        let s = select(
            &t,
            &inp(Tier::Simple),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "sonnet");
    }

    #[test]
    fn sol_astra_sol_is_impossible() {
        // OpenAI-only demanding refactoring: Sol fails on the demanding
        // ladder, Astra fails after it. The retry must not climb another
        // ladder back to Sol.
        let t = vec![
            entry("simple", &["luna", "sol"]),
            entry("demanding", &["astra", "sol"]),
        ];
        let steer = Steer {
            above: Some("astra".into()),
            escalations: 2,
            cell: Some(("debugging".into(), "demanding".into())),
            attempted: vec!["sol".into(), "astra".into()],
            ..Default::default()
        };
        let r = select(
            &t,
            &inp(Tier::Demanding),
            Mode::Balanced,
            &RoutingConfig::default(),
            &|_| true,
            &steer,
        );
        assert!(r.is_err(), "{r:?}");
    }

    #[test]
    fn a_retry_climbs_by_measured_quality_not_cost() {
        // Cost order a, b, c; quality order a (0.5) < c (0.7) < b (0.9).
        let mut t = vec![entry("normal", &["a", "b", "c"])];
        t[0].ladder[0].pass_rate = 0.5;
        t[0].ladder[1].pass_rate = 0.9;
        t[0].ladder[2].pass_rate = 0.7;
        let run = |above: &str, tried: &[&str]| {
            select(
                &t,
                &inp(Tier::Normal),
                Mode::Balanced,
                &RoutingConfig::default(),
                &|_| true,
                &Steer {
                    above: Some(above.into()),
                    escalations: 1,
                    attempted: tried.iter().map(|m| m.to_string()).collect(),
                    ..Default::default()
                },
            )
            .map(|s| s.unwrap().model)
        };
        assert_eq!(run("a", &["a"]).unwrap(), "c");
        assert_eq!(run("c", &["a", "c"]).unwrap(), "b");
        assert!(run("b", &["a", "c", "b"]).is_err());
    }
    #[test]
    fn best_takes_the_highest_measured_quality_not_the_dearest_rung() {
        let mut t = vec![entry("normal", &["a", "b", "c"])];
        t[0].ladder[0].pass_rate = 0.5;
        t[0].ladder[1].pass_rate = 0.9;
        t[0].ladder[2].pass_rate = 0.7;
        let s = select(
            &t,
            &inp(Tier::Normal),
            Mode::Best,
            &RoutingConfig::default(),
            &|_| true,
            &Steer::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(s.model, "b");
    }

    #[test]
    fn top2_takes_the_dearer_of_the_two_picks() {
        let mut t = vec![
            entry("simple", &["pricey"]),
            entry("normal", &["cheap", "other"]),
        ];
        let cfg = RoutingConfig {
            tier_hedge: TierHedge::Top2,
            ..Default::default()
        };
        let run = |t: &[TableEntry]| {
            let i = Inputs {
                confidence: Some(0.1),
                neighbour: Some(Tier::Normal),
                ..inp(Tier::Simple)
            };
            select(t, &i, Mode::Balanced, &cfg, &|_| true, &Steer::default())
                .unwrap()
                .unwrap()
        };
        t[0].ladder[0].cost_usd = Some(0.5);
        t[1].ladder[0].cost_usd = Some(0.01);
        assert_eq!(run(&t).model, "pricey", "the lower tier's pick is dearer");
        t[0].ladder[0].cost_usd = Some(0.005);
        assert_eq!(run(&t).model, "cheap", "the neighbour's pick is dearer");
        t[0].ladder[0].cost_usd = None;
        assert_eq!(run(&t).model, "cheap", "no costs: the higher tier");
    }

    #[test]
    fn partial_evidence_is_refused_and_stale_effort_is_dropped() {
        let basis = |status: &str, effort: &str| Basis {
            status: status.into(),
            effort: effort.into(),
            harness: "h1".into(),
            task_set: "abc".into(),
            measured: "2026-11-14".into(),
        };
        let mut t = vec![entry("normal", &["a", "b"])];
        t[0].ladder[0].basis = Some(basis("partial", ""));
        assert!(validate(&t, |_| true).is_err());
        t[0].ladder[0].basis = Some(basis("complete", "high"));
        t[0].ladder[1].basis = Some(basis("complete", ""));
        validate(&t, |_| true).unwrap();
        // "a" now runs at medium: its rung measured at high is stale.
        let stale = prune_stale(&mut t, |_| Some("medium".into()), true);
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(t.is_empty());
        let mut t = vec![entry("normal", &["a", "b"])];
        t[0].ladder[0].basis = Some(basis("complete", "medium"));
        let stale = prune_stale(&mut t, |_| Some("medium".into()), true);
        assert!(stale.is_empty());
        assert_eq!(t[0].ladder.len(), 2);
    }
}
