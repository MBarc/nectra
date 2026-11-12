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
    let block = render(table)?;
    match (registry_text.find(BEGIN), registry_text.find(END)) {
        (Some(a), Some(b)) if a < b => {
            let end = b + END.len();
            let end = registry_text[end..]
                .strip_prefix('\n')
                .map_or(end, |_| end + 1);
            Ok(format!(
                "{}{block}{}",
                &registry_text[..a],
                &registry_text[end..]
            ))
        }
        (None, None) => {
            let mut text = registry_text.to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            Ok(format!("{text}\n{block}"))
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

/// How a retry steers the pick.
#[derive(Debug, Clone, Default)]
pub struct Steer {
    /// Step above this model on the ladder: the one that just failed.
    pub above: Option<String>,
    /// Escalations the task has already taken, counting this one.
    pub escalations: u32,
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
    let Some(entry) = find(table, inp.kind, inp.tier) else {
        return Ok(None);
    };
    let ok: Vec<usize> = (0..entry.ladder.len())
        .filter(|&i| usable(&entry.ladder[i].model))
        .collect();
    if steer.escalations > cfg.max_escalations {
        bail!(
            "escalation cap reached ({} step{})",
            cfg.max_escalations,
            if cfg.max_escalations == 1 { "" } else { "s" }
        );
    }
    let Some(&first) = ok.first() else {
        return Ok(None);
    };
    // The cheapest rung for --mode fast, the strongest for best, else the
    // cheapest acceptable one, one higher when the tier is a close call.
    let mut idx = match mode {
        Mode::Fast => first,
        Mode::Best => *ok.last().unwrap_or(&first),
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
                        // The higher tier's cheapest acceptable model.
                        chosen = Some((other, j));
                        hedged = true;
                    }
                }
            }
        }
    }
    // A retry climbs from the model that failed, whatever the mode or the
    // doubt about the tier: one rung above it, never standing still or
    // stepping down. A failed model off this ladder (the user chose it)
    // leaves the pick as it was.
    if let Some(p) = steer
        .above
        .as_ref()
        .and_then(|above| entry.ladder.iter().position(|r| &r.model == above))
    {
        let Some(next) = (p + 1..entry.ladder.len()).find(|&i| usable(&entry.ladder[i].model))
        else {
            bail!(
                "{} is the top of the {} x {} ladder",
                entry.ladder[p].model,
                entry.kind,
                entry.tier
            );
        };
        idx = next;
        chosen = None;
        hedged = false;
    }
    let (e, i) = chosen.unwrap_or((entry, idx));
    let r = &e.ladder[i];
    Ok(Some(Selection {
        model: r.model.clone(),
        kind: e.kind.clone(),
        tier: e.tier.clone(),
        rung: i,
        ladder_len: e.ladder.len(),
        hedged,
        evidence: evidence(r),
    }))
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
            if cfg.embedded {
                bail!("no registry file to write: copy config/models.toml beside your config.toml first");
            }
            let path = cfg.registry_path();
            let current = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let next = splice(&current, &table)?;
            // The whole file must still load as a registry.
            toml::from_str::<toml::Value>(&next).context("the new registry does not parse")?;
            if *dry_run {
                println!(
                    "{} entries, nothing written. {} would hold:\n{}",
                    table.len(),
                    path.display(),
                    render(&table)?
                );
                return Ok(0);
            }
            let tmp = path.with_extension("toml.new");
            std::fs::write(&tmp, &next).with_context(|| format!("writing {}", tmp.display()))?;
            std::fs::rename(&tmp, &path)
                .with_context(|| format!("replacing {}", path.display()))?;
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
        };
        // Hedged start is gpt; one above sonnet is also gpt.
        assert_eq!(pick(&table(), &i, Mode::Balanced, &cfg(), &steer), "gpt");
        let steer = Steer {
            above: Some("sonnet".into()),
            escalations: 1,
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
