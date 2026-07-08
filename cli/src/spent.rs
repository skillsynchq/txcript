//! `txcript spent`: what the sessions in a folder cost, per harness.
//!
//! Per assistant message, in preference order: the cost the harness itself
//! recorded (`Usage::cost_usd` — opencode, pi) is exact; otherwise tokens ×
//! a hardcoded price table estimate it (`*`); otherwise the spend is unknown
//! (`?`). The total is a floor (`+`) whenever anything stayed unknown.

use txcript::{HarnessId, common};

/// Model-id prefix → $/Mtok as `[input, cache_read, cache_write, output]`.
///
/// Anthropic bills cache reads at 0.1× input and cache writes at 1.25×
/// input (the 5-minute-TTL rate; transcripts don't record the TTL).
/// OpenAI bills cached input at its own discounted rate and never bills
/// cache writes. Sonnet 5's introductory discount is ignored.
///
/// Rates as of 2026-07 (Anthropic model docs; OpenAI pricing page and the
/// LiteLLM registry for retired ids). Lookup is longest-prefix, so dated
/// ids (`claude-opus-4-8-20250601`) hit their family row and
/// `gpt-5.3-codex-spark` prices as `gpt-5.3-codex` — close enough for `*`.
const PRICES: &[(&str, [f64; 4])] = &[
    ("claude-fable-5", [10.0, 1.0, 12.5, 50.0]),
    ("claude-mythos-5", [10.0, 1.0, 12.5, 50.0]),
    ("claude-opus-4-8", [5.0, 0.5, 6.25, 25.0]),
    ("claude-opus-4-7", [5.0, 0.5, 6.25, 25.0]),
    ("claude-opus-4-6", [5.0, 0.5, 6.25, 25.0]),
    ("claude-opus-4-5", [5.0, 0.5, 6.25, 25.0]),
    ("claude-opus-4-1", [15.0, 1.5, 18.75, 75.0]),
    // opencode spells opus 4.1 without the dash
    ("claude-opus-41", [15.0, 1.5, 18.75, 75.0]),
    ("claude-sonnet-5", [3.0, 0.3, 3.75, 15.0]),
    // covers sonnet 4, 4-5, and 4-6 — all $3/$15
    ("claude-sonnet-4", [3.0, 0.3, 3.75, 15.0]),
    ("claude-haiku-4-5", [1.0, 0.1, 1.25, 5.0]),
    // claude_code sometimes records the bare alias instead of an id
    ("fable", [10.0, 1.0, 12.5, 50.0]),
    ("opus", [5.0, 0.5, 6.25, 25.0]),
    ("sonnet", [3.0, 0.3, 3.75, 15.0]),
    ("haiku", [1.0, 0.1, 1.25, 5.0]),
    // the pro tiers have no cached-input discount
    ("gpt-5.5-pro", [30.0, 30.0, 0.0, 180.0]),
    ("gpt-5.5", [5.0, 0.5, 0.0, 30.0]),
    ("gpt-5.4-pro", [30.0, 30.0, 0.0, 180.0]),
    ("gpt-5.4-mini", [0.75, 0.075, 0.0, 4.5]),
    ("gpt-5.4-nano", [0.2, 0.02, 0.0, 1.25]),
    ("gpt-5.4", [2.5, 0.25, 0.0, 15.0]),
    ("gpt-5.3-codex", [1.75, 0.175, 0.0, 14.0]),
    // gpt-5.2 and gpt-5.2-codex share rates
    ("gpt-5.2", [1.75, 0.175, 0.0, 14.0]),
    ("gpt-5-mini", [0.25, 0.025, 0.0, 2.0]),
    // gpt-5 and gpt-5-codex share rates
    ("gpt-5", [1.25, 0.125, 0.0, 10.0]),
];

/// The longest price-table prefix of `model`, if any.
fn rates(model: &str) -> Option<&'static [f64; 4]> {
    PRICES
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, r)| r)
}

/// All tokens the turn touched, cache traffic included. `Usage` is
/// canonical fresh-input across harnesses, so this is a plain sum.
fn turn_tokens(u: &common::Usage) -> u64 {
    u.input_tokens
        + u.output_tokens
        + u.cache_read_input_tokens.unwrap_or(0)
        + u.cache_creation_input_tokens.unwrap_or(0)
}

/// Tokens × the model's rates, in dollars.
fn estimate(model: &str, u: &common::Usage) -> Option<f64> {
    rates(model).map(|[input, read, write, output]| {
        let m = |tokens: u64, rate: f64| tokens as f64 * rate / 1e6;
        m(u.input_tokens, *input)
            + m(u.cache_read_input_tokens.unwrap_or(0), *read)
            + m(u.cache_creation_input_tokens.unwrap_or(0), *write)
            + m(u.output_tokens, *output)
    })
}

/// One assistant turn's spend, best knowledge first.
enum Cost {
    /// The harness recorded the dollars itself.
    Recorded(f64),
    /// Priced from tokens; approximate.
    Estimated(f64),
    /// Usage exists but no recorded cost and no price for the model.
    Unknown,
}

fn message_cost(meta: &common::Meta, msg: &common::Message) -> Option<Cost> {
    msg.usage.as_ref().map(|u| {
        let model = msg.model.as_deref().or(meta.model.as_deref());
        match (u.cost_usd, model.and_then(|m| estimate(m, u))) {
            (Some(recorded), _) => Cost::Recorded(recorded),
            (None, Some(estimated)) => Cost::Estimated(estimated),
            (None, None) => Cost::Unknown,
        }
    })
}

/// Everything one harness's sessions add up to.
#[derive(Default)]
struct Agg {
    sessions: usize,
    tokens: u64,
    dollars: f64,
    /// Any dollar in `dollars` came from a token estimate.
    estimated: bool,
    /// Sessions that yielded no cost information at all.
    unknown_sessions: usize,
    /// Some turns in otherwise-priced sessions stayed unpriced.
    partial: bool,
}

impl Agg {
    /// Fold one session's messages in.
    fn add_session(&mut self, t: &txcript::Transcript<txcript::Common>) {
        self.sessions += 1;
        let mut priced = false;
        let mut unpriced = false;
        for msg in &t.body {
            if let Some(u) = msg.usage.as_ref() {
                self.tokens += turn_tokens(u);
            }
            match message_cost(&t.meta, msg) {
                // A turn with no usage carries no spend at all.
                None => {}
                Some(Cost::Recorded(d)) => {
                    self.dollars += d;
                    priced = true;
                }
                Some(Cost::Estimated(d)) => {
                    self.dollars += d;
                    self.estimated = true;
                    priced = true;
                }
                Some(Cost::Unknown) => unpriced = true,
            }
        }
        match (priced, unpriced) {
            // Fully priced: nothing to flag.
            (true, false) => {}
            (true, true) => self.partial = true,
            (false, _) => self.unknown_sessions += 1,
        }
    }
}

pub(super) fn cmd_spent(from: Option<HarnessId>, cwd: Option<&std::path::Path>) {
    let sessions = super::discover_with_spinner();
    let mut per: Vec<(HarnessId, Agg)> = Vec::new();
    let scoped = sessions.iter().filter(|s| super::selected(s, from, cwd));
    for session in scoped {
        // Unreadable sessions are skipped, matching list and query.
        if let Ok(common) = session.read() {
            let agg = per
                .iter_mut()
                .find(|(h, _)| *h == session.harness)
                .map(|(_, a)| a);
            match agg {
                Some(agg) => agg.add_session(&common),
                None => {
                    let mut agg = Agg::default();
                    agg.add_session(&common);
                    per.push((session.harness, agg));
                }
            }
        }
    }
    render(&per, cwd);
}

fn render(per: &[(HarnessId, Agg)], cwd: Option<&std::path::Path>) {
    if per.is_empty() {
        let scope = cwd.map_or(String::new(), |d| format!(" for {}", d.display()));
        println!("no local sessions found{scope}");
    } else {
        let mut rows: Vec<_> = per.iter().collect();
        // Big spenders first; all-unknown rows sink to the bottom.
        rows.sort_by(|(_, a), (_, b)| {
            b.dollars
                .partial_cmp(&a.dollars)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let color = super::style::enabled();
        let header = format!("{:<12}  {:>8}  {:>10}  {:>12}", "HARNESS", "SESSIONS", "TOKENS", "SPENT");
        println!("{}", super::style::dim(&header, color));
        for (harness, agg) in &rows {
            println!(
                "{}  {:>8}  {:>10}  {:>12}",
                super::style::harness(*harness, 12, color),
                agg.sessions,
                humanize(agg.tokens),
                spent_cell(agg),
            );
        }

        let total: f64 = rows.iter().map(|(_, a)| a.dollars).sum();
        let floor = rows
            .iter()
            .any(|(_, a)| a.unknown_sessions > 0 || a.partial);
        let marker = if floor { "+" } else { "" };
        println!();
        println!("{:<12}  {:>8}  {:>10}  {:>12}", "total", "", "", format!("${total:.2}{marker}"));
        let estimated = rows.iter().any(|(_, a)| a.estimated);
        if estimated || floor {
            let mut notes: Vec<&str> = Vec::new();
            if estimated {
                notes.push("* estimated from token prices");
            }
            if floor {
                notes.push("? no cost data · + total is a floor");
            }
            println!("{}", super::style::dim(&notes.join(" · "), color));
        }
    }
}

/// The SPENT column for one harness row.
fn spent_cell(agg: &Agg) -> String {
    let mark = if agg.estimated { "*" } else { "" };
    match (agg.dollars > 0.0, agg.unknown_sessions > 0 || agg.partial) {
        (true, true) => format!("${:.2}{mark}+", agg.dollars),
        (true, false) => format!("${:.2}{mark}", agg.dollars),
        (false, _) => "?".into(),
    }
}

/// `41_234_567` → `41.2M`, `985_300` → `985.3K`, `123` → `123`.
fn humanize(tokens: u64) -> String {
    match tokens {
        0 => "—".into(),
        t if t >= 1_000_000 => format!("{:.1}M", t as f64 / 1e6),
        t if t >= 1_000 => format!("{:.1}K", t as f64 / 1e3),
        t => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, read: Option<u64>, write: Option<u64>) -> common::Usage {
        common::Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: read,
            cache_creation_input_tokens: write,
            cost_usd: None,
        }
    }

    #[test]
    fn lookup_prefers_the_longest_prefix() {
        // `gpt-5.5` must not fall through to the `gpt-5` row.
        assert_eq!(rates("gpt-5.5"), Some(&[5.0, 0.5, 0.0, 30.0]));
        assert_eq!(rates("gpt-5-codex"), Some(&[1.25, 0.125, 0.0, 10.0]));
        // Dated ids hit their family row.
        assert_eq!(
            rates("claude-haiku-4-5-20251001"),
            Some(&[1.0, 0.1, 1.25, 5.0])
        );
        assert_eq!(rates("harmonic-relay"), None);
    }

    #[test]
    fn estimate_prices_each_token_kind_at_its_own_rate() {
        // Usage is canonical fresh-input: 1M fresh in, 1M out, 1M read, 1M
        // written on opus-4-8 is 5 + 25 + 0.5 + 6.25.
        let u = usage(1_000_000, 1_000_000, Some(1_000_000), Some(1_000_000));
        let d = estimate("claude-opus-4-8", &u).unwrap();
        assert!((d - 36.75).abs() < 1e-9);

        // OpenAI rates: cached input discounted, cache writes free — 400K
        // fresh + 600K cached + 1M out on gpt-5: 0.5 + 0.075 + 10.
        let u = usage(400_000, 1_000_000, Some(600_000), None);
        let d = estimate("gpt-5", &u).unwrap();
        assert!((d - 10.575).abs() < 1e-9);
    }

    #[test]
    fn recorded_cost_beats_the_estimate() {
        let meta = common::Meta {
            id: "s".into(),
            timestamp: "2026-01-02T03:04:05Z".parse().unwrap(),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: Some("claude-opus-4-8".into()),
        };
        let mut msg = common::Message {
            role: common::Role::Assistant,
            content: vec![],
            timestamp: "2026-01-02T03:04:05Z".parse().unwrap(),
            model: None,
            stop_reason: None,
            usage: Some(common::Usage {
                cost_usd: Some(0.42),
                ..usage(1_000_000, 0, None, None)
            }),
        };
        assert!(matches!(
            message_cost(&meta, &msg),
            Some(Cost::Recorded(d)) if (d - 0.42).abs() < 1e-9
        ));

        // Without a recorded cost the priced model estimates...
        msg.usage.as_mut().unwrap().cost_usd = None;
        assert!(matches!(
            message_cost(&meta, &msg),
            Some(Cost::Estimated(d)) if (d - 5.0).abs() < 1e-9
        ));

        // ...and an unpriced model is unknown, not $0.
        msg.model = Some("harmonic-relay".into());
        let meta = common::Meta { model: None, ..meta };
        assert!(matches!(
            message_cost(&meta, &msg),
            Some(Cost::Unknown)
        ));
    }

    #[test]
    fn formatting() {
        assert_eq!(humanize(0), "—");
        assert_eq!(humanize(123), "123");
        assert_eq!(humanize(985_300), "985.3K");
        assert_eq!(humanize(41_234_567), "41.2M");
    }
}
