// Pricing: https://platform.claude.com/docs/en/about-claude/pricing
// Cache writes bill by TTL — 5m = 1.25x base input, 1h = 2x; subscription
// users default to 1h, so flat-5m would materially undercount. A rate that
// depends on the request (speed, size, date) lives in `VARIANTS`.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::aggregator::TokenStats;

static GLOBAL_CALCULATOR: LazyLock<CostCalculator> = LazyLock::new(CostCalculator::new);

const FAMILIES: &[(&str, &str)] = &[
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
    ("fable", "Fable"),
    ("mythos", "Mythos"),
];

pub fn normalize_model_name(model: &str) -> String {
    let model = model_of_key(model);
    for &(family, display) in FAMILIES {
        if !model.contains(family) {
            continue;
        }

        let prefix = format!("{family}-");
        if let Some(pos) = model.find(&prefix) {
            let after = &model[pos + prefix.len()..];
            let major_str: String = after.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(major) = major_str.parse::<u32>()
                && major_str.len() <= 2
            {
                let rest = &after[major_str.len()..];
                if let Some(rest) = rest.strip_prefix('-') {
                    let minor_str: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    if minor_str.len() <= 2
                        && let Ok(minor) = minor_str.parse::<u32>()
                    {
                        return format!("{display} {major}.{minor}");
                    }
                }
                return format!("{display} {major}");
            }
        }

        if let Some(pos) = model.find(family) {
            let before = &model[..pos];
            let parts: Vec<&str> = before.split('-').filter(|s| !s.is_empty()).collect();
            let nums: Vec<u32> = parts
                .iter()
                .rev()
                .take(2)
                .filter_map(|s| s.parse::<u32>().ok())
                .collect();
            match nums.len() {
                2 => return format!("{} {}.{}", display, nums[1], nums[0]),
                1 => return format!("{} {}", display, nums[0]),
                _ => {}
            }
        }

        // Family matched but no version digits (e.g. a "-preview" id): keep
        // the raw name. Collapsing to the bare family label would merge it
        // with versioned siblings' namespace and hide WHICH id is unpriced.
        return model.to_string();
    }

    // Unknown model family — keep the raw API name so the UI can show it individually
    // (and flag it with a "no pricing" badge) rather than collapsing every unknown
    // model into a single "Other" bucket. Empty input falls back to literal "unknown".
    if model.is_empty() {
        "unknown".to_string()
    } else {
        model.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_cost_per_mtok: f64,
    pub output_cost_per_mtok: f64,
    /// 5-minute TTL cache write rate = 1.25x base input. API key auth and
    /// Bedrock / Vertex / Foundry default to this TTL.
    pub cache_write_5m_cost_per_mtok: f64,
    /// 1-hour TTL cache write rate = 2x base input. Subscription users
    /// (Pro/Max/Team via Claude Code) get this TTL automatically.
    pub cache_write_1h_cost_per_mtok: f64,
    pub cache_read_cost_per_mtok: f64, // 0.1x input
}

/// `(model id, input, output, 5m cache write, 1h cache write, cache read)`,
/// $/MTok, from platform.claude.com/docs/en/about-claude/pricing. Rates are
/// explicit rather than derived from multipliers: several rows differ from
/// the usual 1.25x / 2x / 0.1x.
#[rustfmt::skip]
const PRICES: &[(&str, f64, f64, f64, f64, f64)] = &[
    // 5.1 reads bill at 0.025x base, not 0.1x: a sibling's row overcharges them.
    ("claude-fable-5-1", 10.0, 50.0, 12.5, 20.0, 0.25),
    ("claude-mythos-5-1", 10.0, 50.0, 12.5, 20.0, 0.25),
    ("claude-fable-5", 10.0, 50.0, 12.5, 20.0, 1.0),
    // Offered through Project Glasswing, so it can appear in real data.
    ("claude-mythos-5", 10.0, 50.0, 12.5, 20.0, 1.0),
    // Opus / Sonnet 5.5 reads bill at 0.05x base.
    ("claude-opus-5-5", 4.0, 20.0, 5.0, 8.0, 0.20),
    ("claude-opus-5", 5.0, 25.0, 6.25, 10.0, 0.50),
    ("claude-opus-4-8", 5.0, 25.0, 6.25, 10.0, 0.50),
    ("claude-opus-4-7", 5.0, 25.0, 6.25, 10.0, 0.50),
    ("claude-opus-4-6", 5.0, 25.0, 6.25, 10.0, 0.50),
    ("claude-opus-4-5", 5.0, 25.0, 6.25, 10.0, 0.50),
    ("claude-opus-4-1", 15.0, 75.0, 18.75, 30.0, 1.50),
    ("claude-opus-4", 15.0, 75.0, 18.75, 30.0, 1.50),
    // Reads before the cut sit in `VARIANTS`.
    ("claude-sonnet-5-5", 2.0, 10.0, 2.50, 4.0, 0.10),
    // Below Sonnet 4.6: its launch rate is its standard rate.
    ("claude-sonnet-5", 2.0, 10.0, 2.50, 4.0, 0.20),
    // Requests over its input tier sit in `VARIANTS`.
    ("claude-haiku-5-5", 0.10, 0.50, 0.125, 0.20, 0.01),
    ("claude-sonnet-4-6", 3.0, 15.0, 3.75, 6.0, 0.30),
    ("claude-sonnet-4-5", 3.0, 15.0, 3.75, 6.0, 0.30),
    ("claude-sonnet-4", 3.0, 15.0, 3.75, 6.0, 0.30),
    ("claude-3-7-sonnet", 3.0, 15.0, 3.75, 6.0, 0.30),
    ("claude-3-5-sonnet", 3.0, 15.0, 3.75, 6.0, 0.30),
    ("claude-3-opus", 15.0, 75.0, 18.75, 30.0, 1.50),
    ("claude-haiku-4-5", 1.0, 5.0, 1.25, 2.0, 0.10),
    ("claude-3-5-haiku", 0.80, 4.0, 1.0, 1.60, 0.08),
    // The 1h write rate is inferred as 2x base; the table lists none.
    ("claude-3-haiku", 0.25, 1.25, 0.30, 0.50, 0.03),
];

/// Whether one request bills at a variant row, from its usage and time.
type Applies = fn(&crate::domain::Usage, Option<chrono::DateTime<chrono::Utc>>) -> bool;

/// `(pricing key, tag, label, applies, rates as in PRICES)`: a request billed off
/// its model's base row. The first row of the request's model whose
/// `applies` holds sets its tag; a model with no such row bills at base.
/// Tagged keys are stored in the cache: adding, removing or retagging a row,
/// or changing an `applies`, needs a `CACHE_VERSION` bump.
#[rustfmt::skip]
const VARIANTS: &[(&str, &str, &str, Applies, f64, f64, f64, f64, f64)] = &[
    // Fast mode: a dearer base, with the model's usual cache multipliers.
    // A model without a row here runs a fast request at standard speed.
    ("claude-opus-5-5", "fast", "fast mode", is_fast, 8.0, 40.0, 10.0, 16.0, 0.40),
    ("claude-opus-5", "fast", "fast mode", is_fast, 10.0, 50.0, 12.5, 20.0, 1.0),
    ("claude-opus-4-8", "fast", "fast mode", is_fast, 10.0, 50.0, 12.5, 20.0, 1.0),
    // The whole request, output included, bills at the upper tier.
    ("claude-haiku-5-5", "over-100k", "prompts over 100K", over_haiku_5_5_tier, 0.50, 2.50, 0.625, 1.0, 0.05),
    ("claude-sonnet-5-5", "launch-reads", "reads before the cut", before_sonnet_5_5_read_cut, 2.0, 10.0, 2.50, 4.0, 0.20),
];

fn is_fast(usage: &crate::domain::Usage, _: Option<chrono::DateTime<chrono::Utc>>) -> bool {
    usage.speed.as_deref() == Some("fast")
}

/// Priced by prompt length: input + cache read + cache write, never output.
const HAIKU_5_5_TIER_TOKENS: u64 = 100_000;

fn over_haiku_5_5_tier(
    usage: &crate::domain::Usage,
    _: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
        > HAIKU_5_5_TIER_TOKENS
}

/// The announcement names the day only; UTC matches the Usage API buckets.
static SONNET_5_5_READ_CUT: LazyLock<chrono::DateTime<chrono::Utc>> = LazyLock::new(|| {
    "2026-10-07T00:00:00Z" // lint-ok: date-literal
        .parse()
        .expect("valid RFC 3339 literal")
});

fn before_sonnet_5_5_read_cut(
    _: &crate::domain::Usage,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    at.is_some_and(|t| t < *SONNET_5_5_READ_CUT)
}

const TAG_SEP: char = '+';

/// What a pricing key's rate is, in words: `standard` for a model's base row.
pub(crate) fn pricing_label(key: &str) -> &'static str {
    match key.split_once(TAG_SEP) {
        None => "standard",
        Some((_, tag)) => VARIANTS
            .iter()
            .find(|row| row.1 == tag)
            .map_or("other rate", |row| row.2),
    }
}

/// The model id a pricing key was built from, its tag dropped.
pub(crate) fn model_of_key(key: &str) -> &str {
    key.split_once(TAG_SEP).map_or(key, |(model, _)| model)
}

/// The key one request bills under: its model, tagged when a `VARIANTS`
/// row of that model applies to the request.
pub(crate) fn pricing_key(
    model: &str,
    usage: &crate::domain::Usage,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let Some(base) = CostCalculator::global().resolved_key(model) else {
        return model.to_string();
    };
    VARIANTS
        .iter()
        .find(|row| row.0 == base && (row.3)(usage, at))
        .map_or_else(
            || model.to_string(),
            |row| format!("{model}{TAG_SEP}{}", row.1),
        )
}

/// Every billed attempt of one response as `(pricing key, usage)`. The
/// top-level usage is the returned attempt; a declined attempt that produced
/// output bills too, at its own model, while one without output is free.
pub(crate) fn billed_attempts(
    model: &str,
    usage: &crate::domain::Usage,
    at: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<(String, crate::domain::Usage)> {
    let returned = crate::domain::Usage {
        iterations: Vec::new(),
        ..usage.clone()
    };
    let declined = usage
        .iterations
        .split_last()
        .map_or(&[][..], |(_, earlier)| earlier)
        .iter()
        .filter(|it| it.output_tokens > 0)
        .filter_map(|it| {
            // An attempt without a model ran on the reply's; a synthetic
            // one is not billed, as for the reply itself.
            let model = it
                .model
                .as_deref()
                .filter(|m| !m.is_empty())
                .unwrap_or(model);
            let attempt = it.as_usage(usage);
            crate::aggregator::is_real_model(model)
                .then(|| (pricing_key(model, &attempt, at), attempt))
        });
    std::iter::once((pricing_key(model, &returned, at), returned))
        .chain(declined)
        .collect()
}

pub struct CostCalculator {
    pricing: HashMap<String, ModelPricing>,
    sorted_keys: Vec<String>,
    /// Model name → the pricing key it matched (`None`: unpriced). The match
    /// scans every key, and the views price each session on every frame,
    /// while a log names only a few dozen models.
    resolved: std::sync::RwLock<HashMap<String, Option<String>>>,
}

impl Default for CostCalculator {
    fn default() -> Self {
        Self::new()
    }
}

impl CostCalculator {
    pub fn global() -> &'static CostCalculator {
        &GLOBAL_CALCULATOR
    }

    pub fn new() -> Self {
        let mut pricing = HashMap::new();
        for &(id, input, output, write_5m, write_1h, read) in PRICES {
            pricing.insert(
                id.to_string(),
                ModelPricing {
                    input_cost_per_mtok: input,
                    output_cost_per_mtok: output,
                    cache_write_5m_cost_per_mtok: write_5m,
                    cache_write_1h_cost_per_mtok: write_1h,
                    cache_read_cost_per_mtok: read,
                },
            );
        }

        let mut sorted_keys: Vec<String> = pricing.keys().cloned().collect();
        sorted_keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        for &(base, tag, _, _, input, output, write_5m, write_1h, read) in VARIANTS {
            pricing.insert(
                format!("{base}{TAG_SEP}{tag}"),
                ModelPricing {
                    input_cost_per_mtok: input,
                    output_cost_per_mtok: output,
                    cache_write_5m_cost_per_mtok: write_5m,
                    cache_write_1h_cost_per_mtok: write_1h,
                    cache_read_cost_per_mtok: read,
                },
            );
        }

        Self {
            pricing,
            sorted_keys,
            resolved: std::sync::RwLock::default(),
        }
    }

    pub fn calculate_cost(&self, tokens: &TokenStats, model: Option<&str>) -> Option<f64> {
        if tokens.non_standard_speed {
            return None;
        }
        let pricing = self.get_pricing(model)?;
        let million = 1_000_000.0;

        let input_cost = tokens.input_tokens as f64 / million * pricing.input_cost_per_mtok;
        let output_cost = tokens.output_tokens as f64 / million * pricing.output_cost_per_mtok;
        // Apply per-TTL rates from the structured `cache_creation` breakdown.
        // For entries that pre-date the structured field, the aggregator's
        // `TokenStats::add` routes all flat cache writes into the 5m bucket,
        // which matches Anthropic's API key default — so this branch is
        // already correct for legacy data.
        let cache_5m_cost =
            tokens.cache_creation_5m_tokens as f64 / million * pricing.cache_write_5m_cost_per_mtok;
        let cache_1h_cost =
            tokens.cache_creation_1h_tokens as f64 / million * pricing.cache_write_1h_cost_per_mtok;
        let cache_read_cost =
            tokens.cache_read_tokens as f64 / million * pricing.cache_read_cost_per_mtok;

        Some(input_cost + output_cost + cache_5m_cost + cache_1h_cost + cache_read_cost)
    }

    pub fn get_pricing_by_display_name(&self, display_name: &str) -> Option<&ModelPricing> {
        for key in &self.sorted_keys {
            if normalize_model_name(key) == display_name {
                return self.pricing.get(key);
            }
        }
        None
    }

    pub fn has_pricing(&self, model: Option<&str>) -> bool {
        self.get_pricing(model).is_some()
    }

    /// Whether these tokens' cost is unknown: the model has no row, or a
    /// message ran at a speed with no rate. The one predicate behind every
    /// `$?` / `*` mark, so a mark and a `None` cost never disagree.
    pub fn is_unpriced(&self, model: &str, tokens: &TokenStats) -> bool {
        self.calculate_cost(tokens, Some(model)).is_none()
    }

    pub fn models_without_pricing(
        &self,
        model_tokens: &std::collections::HashMap<String, TokenStats>,
    ) -> std::collections::HashSet<String> {
        let mut result = std::collections::HashSet::new();
        for (model, tokens) in model_tokens {
            if self.is_unpriced(model, tokens) {
                result.insert(Self::simplify_model_name(model));
            }
        }
        result
    }

    /// Cost per display model over `(model, tokens)` pairs, priced pair by
    /// pair. Pass per-slice pairs, not a pre-merged map: merging makes one
    /// unpriced message (an unknown speed tier) unprice the model's whole range.
    pub fn calculate_costs_by_model<'a>(
        &self,
        model_tokens: impl IntoIterator<Item = (&'a String, &'a TokenStats)>,
    ) -> Vec<(String, f64)> {
        let mut aggregated: HashMap<String, f64> = HashMap::new();

        for (model, tokens) in model_tokens {
            let simplified = Self::simplify_model_name(model);
            let cost = self.calculate_cost(tokens, Some(model)).unwrap_or(0.0);
            *aggregated.entry(simplified).or_insert(0.0) += cost;
        }

        let mut costs: Vec<(String, f64)> = aggregated.into_iter().collect();
        costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        costs
    }

    pub fn aggregate_tokens_by_model(
        model_tokens: &std::collections::HashMap<String, TokenStats>,
    ) -> std::collections::HashMap<String, TokenStats> {
        let mut aggregated: HashMap<String, TokenStats> = HashMap::new();

        for (model, tokens) in model_tokens {
            aggregated
                .entry(Self::simplify_model_name(model))
                .or_default()
                .merge(tokens);
        }

        aggregated
    }

    /// Bucket name for a model in the cost / token-by-model views. Synthetic
    /// (`<...>`) and empty models collapse into one `Other` bucket (they
    /// can't be priced); everything else uses its normalized display name.
    /// `Other` is the unpriceable bucket, distinct from `normalize_model_name`'s
    /// `unknown` display fallback.
    fn simplify_model_name(model: &str) -> String {
        if model.starts_with('<') || model.is_empty() {
            return "Other".to_string();
        }
        normalize_model_name(model)
    }

    pub fn get_pricing(&self, model: Option<&str>) -> Option<&ModelPricing> {
        self.resolved_key(model?).and_then(|k| self.pricing.get(&k))
    }

    /// The pricing row a model name or pricing key matches, memoized.
    fn resolved_key(&self, model_name: &str) -> Option<String> {
        if let Ok(resolved) = self.resolved.read()
            && let Some(key) = resolved.get(model_name)
        {
            return key.clone();
        }
        let key = self.match_pricing_key(model_name);
        if let Ok(mut resolved) = self.resolved.write() {
            resolved.insert(model_name.to_string(), key.clone());
        }
        key
    }

    fn match_pricing_key(&self, key: &str) -> Option<String> {
        let base = self.base_key(model_of_key(key))?;
        match key.split_once(TAG_SEP) {
            Some((_, tag)) => {
                let tagged = format!("{base}{TAG_SEP}{tag}");
                self.pricing.contains_key(&tagged).then_some(tagged)
            }
            None => Some(base),
        }
    }

    /// The `PRICES` row a bare model id matches.
    fn base_key(&self, model_name: &str) -> Option<String> {
        self.sorted_keys
            .iter()
            .find(|key| {
                model_name
                    .find(key.as_str())
                    .is_some_and(|pos| Self::is_version_boundary(&model_name[pos + key.len()..]))
            })
            .cloned()
    }

    fn is_version_boundary(after: &str) -> bool {
        if after.is_empty() {
            return true;
        }
        // Context-window suffix like `[1m]` (1M-context variant — Cowork emits
        // `claude-opus-4-7[1m]`) is appended directly with no `-` separator, so
        // accept it as a valid boundary. Also covers `(beta)` / `:thinking`
        // style qualifiers if Anthropic adds them later. Without this, the
        // model fails pricing lookup and cost silently rounds to $0.
        if matches!(after.chars().next(), Some('[' | '(' | ':')) {
            return true;
        }
        let Some(rest) = after.strip_prefix('-') else {
            return false;
        };
        let digit_count = rest.chars().take_while(char::is_ascii_digit).count();
        // 1-3 digits after '-' = version continuation (e.g. "-6", "-45")
        // 4+ digits = date suffix (e.g. "-20260101")
        // 0 digits = other suffix, treat as boundary
        digit_count == 0 || digit_count >= 4
    }
}

#[cfg(test)]
mod tests {

    // The memo only skips the key scan: a repeat lookup lands on the same
    // pricing as the first, priced or not.
    #[test]
    fn a_repeated_pricing_lookup_matches_the_first() {
        let calc = CostCalculator::new();
        for model in [
            "claude-opus-4-6",
            "claude-sonnet-4-20250514",
            "claude-future-x",
            "gpt-4",
        ] {
            let first = calc.get_pricing(Some(model)).map(std::ptr::from_ref);
            let again = calc.get_pricing(Some(model)).map(std::ptr::from_ref);
            assert_eq!(first, again, "{model}");
            assert_eq!(
                first,
                calc.match_pricing_key(model)
                    .and_then(|k| calc.pricing.get(&k))
                    .map(std::ptr::from_ref),
                "{model}"
            );
        }
        assert!(calc.get_pricing(Some("claude-future-x")).is_none());
    }
    use super::*;

    #[test]
    fn test_known_models_have_pricing() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(Some("claude-sonnet-4")).is_some());
        assert!(calculator.get_pricing(Some("claude-opus-4-5")).is_some());
        assert!(calculator.get_pricing(Some("claude-opus-4-8")).is_some());
        assert!(calculator.get_pricing(Some("claude-fable-5")).is_some());
        assert!(calculator.get_pricing(Some("claude-mythos-5")).is_some());
        // Mythos Preview has no published rates — it must stay unpriced (the
        // UI marks it "$?") rather than borrowing Mythos 5's, and must not
        // substring-match the claude-mythos-5 entry.
        assert!(
            calculator
                .get_pricing(Some("claude-mythos-preview"))
                .is_none()
        );
    }

    #[test]
    fn test_normalize_fable_and_mythos_families() {
        assert_eq!(normalize_model_name("claude-fable-5"), "Fable 5");
        assert_eq!(normalize_model_name("claude-mythos-5"), "Mythos 5");
        // Version-less family id keeps its raw name — collapsing it to a
        // bare "Mythos" would sit indistinguishably next to "Mythos 5" while
        // carrying different (absent) pricing.
        assert_eq!(
            normalize_model_name("claude-mythos-preview"),
            "claude-mythos-preview"
        );
    }

    #[test]
    fn opus_5_prices_both_the_base_and_1m_context_ids() {
        // The 1M-context variant is the id Claude Code emits for the extended
        // window; it bills at the base rate, so both must resolve to one entry
        // rather than one of them silently costing $0.
        let calculator = CostCalculator::new();
        let base = calculator.get_pricing(Some("claude-opus-5")).unwrap();
        let long_ctx = calculator.get_pricing(Some("claude-opus-5[1m]")).unwrap();
        assert_eq!(base.input_cost_per_mtok, long_ctx.input_cost_per_mtok);
        assert_eq!(base.output_cost_per_mtok, long_ctx.output_cost_per_mtok);
        // Both ids collapse to one badge — a split would double the model row.
        assert_eq!(normalize_model_name("claude-opus-5"), "Opus 5");
        assert_eq!(normalize_model_name("claude-opus-5[1m]"), "Opus 5");
    }

    #[test]
    fn an_unknown_speed_tier_costs_unknown_rather_than_the_standard_rate() {
        let calculator = CostCalculator::new();
        let mut tokens = TokenStats {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Default::default()
        };
        assert_eq!(
            calculator.calculate_cost(&tokens, Some("claude-opus-5")),
            Some(30.0),
            "the standard tier prices normally"
        );
        tokens.non_standard_speed = true;
        assert_eq!(
            calculator.calculate_cost(&tokens, Some("claude-opus-5")),
            None,
            "an unrated speed tier must reach the $? path, not bill at standard"
        );
    }

    #[test]
    fn speed_flag_is_sticky_across_folds() {
        // One flagged message in a day makes the day's cost unknown; folding
        // a clean aggregate afterwards must not clear it.
        let mut day = TokenStats::default();
        day.add(&crate::domain::Usage {
            speed: Some("turbo".to_string()),
            ..Default::default()
        });
        assert!(day.non_standard_speed);
        day.add(&crate::domain::Usage {
            speed: Some("standard".to_string()),
            ..Default::default()
        });
        assert!(
            day.non_standard_speed,
            "a later standard message cleared it"
        );

        let mut total = TokenStats::default();
        total.merge(&day);
        assert!(total.non_standard_speed, "merge dropped the flag");
    }

    #[test]
    fn fable_5_1_reads_bill_at_the_reduced_cache_multiplier() {
        // Same base as Fable 5 but a quarter of its cache-read rate. Cache
        // reads dominate real volume, so cloning the sibling entry would
        // silently overcharge every session on this model.
        let calculator = CostCalculator::new();
        let f51 = calculator.get_pricing(Some("claude-fable-5-1")).unwrap();
        let f5 = calculator.get_pricing(Some("claude-fable-5")).unwrap();
        assert_eq!(f51.input_cost_per_mtok, 10.0);
        assert_eq!(f51.output_cost_per_mtok, 50.0);
        assert_eq!(f51.cache_write_5m_cost_per_mtok, 12.5);
        assert_eq!(f51.cache_write_1h_cost_per_mtok, 20.0);
        assert_eq!(f51.cache_read_cost_per_mtok, 0.25);
        assert_eq!(
            f5.cache_read_cost_per_mtok, 1.0,
            "the .0 rate must not move"
        );
        // The longer key has to win, or `-1` falls through to the .0 entry.
        assert_eq!(normalize_model_name("claude-fable-5-1"), "Fable 5.1");
        assert_eq!(normalize_model_name("claude-mythos-5-1"), "Mythos 5.1");
        assert_eq!(
            calculator
                .get_pricing(Some("claude-mythos-5-1"))
                .unwrap()
                .cache_read_cost_per_mtok,
            0.25
        );
    }

    #[test]
    fn sonnet_5_5_is_priced_under_its_own_name() {
        let calculator = CostCalculator::new();
        for id in ["claude-sonnet-5-5", "claude-sonnet-5-5[1m]"] {
            let p = calculator.get_pricing(Some(id)).unwrap();
            assert_eq!(p.input_cost_per_mtok, 2.0);
            assert_eq!(p.output_cost_per_mtok, 10.0);
            assert_eq!(p.cache_write_5m_cost_per_mtok, 2.50);
            assert_eq!(p.cache_write_1h_cost_per_mtok, 4.0);
            assert_eq!(p.cache_read_cost_per_mtok, 0.10);
            assert_eq!(normalize_model_name(id), "Sonnet 5.5");
        }
        // The display-name lookup must find 5.5 itself, not Sonnet 5's entry.
        assert!(
            calculator
                .get_pricing_by_display_name("Sonnet 5.5")
                .is_some()
        );
    }

    fn usage(input: u64, read: u64, write: u64, output: u64) -> crate::domain::Usage {
        crate::domain::Usage {
            input_tokens: input,
            cache_read_input_tokens: read,
            cache_creation_input_tokens: write,
            output_tokens: output,
            ..Default::default()
        }
    }

    fn rates(key: &str) -> (f64, f64, f64, f64, f64) {
        let p = CostCalculator::global().get_pricing(Some(key)).unwrap();
        (
            p.input_cost_per_mtok,
            p.output_cost_per_mtok,
            p.cache_write_5m_cost_per_mtok,
            p.cache_write_1h_cost_per_mtok,
            p.cache_read_cost_per_mtok,
        )
    }

    #[test]
    fn sonnet_5_5_reads_before_the_utc_cut_bill_at_the_launch_rate() {
        let u = usage(1, 0, 0, 1);
        let at = |s: &str| Some(s.parse::<chrono::DateTime<chrono::Utc>>().unwrap());
        for id in ["claude-sonnet-5-5", "claude-sonnet-5-5[1m]"] {
            let before = pricing_key(id, &u, at("2026-10-06T23:59:59Z")); // lint-ok: date-literal
            let after = pricing_key(id, &u, at("2026-10-07T00:00:00Z")); // lint-ok: date-literal
            assert_eq!(rates(&before).4, 0.20, "{before}");
            assert_eq!(rates(&after).4, 0.10, "{after}");
            assert_eq!(rates(&before).0, rates(&after).0, "only reads moved");
            assert_eq!(normalize_model_name(&before), "Sonnet 5.5");
        }
        assert_eq!(
            pricing_key("claude-sonnet-5-5", &u, None),
            "claude-sonnet-5-5"
        );
    }

    #[test]
    fn haiku_5_5_bills_a_whole_request_over_the_tier_at_the_upper_rates() {
        let id = "claude-haiku-5-5";
        let at_tier = pricing_key(id, &usage(40_000, 50_000, 10_000, 9_000), None);
        assert_eq!(
            at_tier, id,
            "the tier is exclusive and output is not counted"
        );
        let over = pricing_key(id, &usage(40_000, 50_000, 10_001, 1), None);
        assert_eq!(rates(&over), (0.50, 2.50, 0.625, 1.0, 0.05));
        assert_eq!(rates(id), (0.10, 0.50, 0.125, 0.20, 0.01));
        assert_eq!(normalize_model_name(&over), "Haiku 5.5");
    }

    #[test]
    fn fast_mode_bills_at_its_rates_and_at_standard_where_it_has_none() {
        let fast = crate::domain::Usage {
            speed: Some("fast".to_string()),
            ..usage(1, 0, 0, 1)
        };
        let key = pricing_key("claude-opus-5-5", &fast, None);
        assert_eq!(rates(&key), (8.0, 40.0, 10.0, 16.0, 0.40));
        assert_eq!(rates(&pricing_key("claude-opus-4-8", &fast, None)).0, 10.0);

        let mut tokens = TokenStats::default();
        tokens.add(&fast);
        assert!(!tokens.non_standard_speed, "fast is a rated speed");
        // A model without fast mode runs the request at standard speed.
        let key = pricing_key("claude-sonnet-4-6", &fast, None);
        assert_eq!(key, "claude-sonnet-4-6");
        assert!(
            CostCalculator::global()
                .calculate_cost(&tokens, Some(&key))
                .is_some()
        );
    }

    // A declined attempt bills at a real model only: one without a model ran
    // on the reply's, and a synthetic one is not billed.
    #[test]
    fn a_declined_attempt_without_a_real_model_bills_as_the_reply_or_not_at_all() {
        let attempt = |model: &str| crate::domain::UsageIteration {
            model: Some(model.to_string()),
            output_tokens: 5,
            ..Default::default()
        };
        let mut u = usage(10, 0, 0, 7);
        u.iterations = vec![
            attempt(""),
            attempt("<synthetic>"),
            crate::domain::UsageIteration::default(),
        ];
        let keys: Vec<String> = billed_attempts("claude-opus-4-8", &u, None)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["claude-opus-4-8", "claude-opus-4-8"]);
    }

    // A declined attempt runs on its own model, so a fast parent request
    // does not make an attempt on a model without fast mode unpriced.
    #[test]
    fn a_declined_attempt_under_a_fast_request_bills_at_its_own_models_speed() {
        let mut u = crate::domain::Usage {
            speed: Some("fast".to_string()),
            ..usage(10, 0, 0, 7)
        };
        u.iterations = vec![
            crate::domain::UsageIteration {
                model: Some("claude-fable-5".to_string()),
                output_tokens: 5,
                ..Default::default()
            },
            crate::domain::UsageIteration::default(),
        ];
        let keys: Vec<String> = billed_attempts("claude-opus-5-5", &u, None)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(keys, ["claude-opus-5-5+fast", "claude-fable-5"]);
        assert!(
            keys.iter()
                .all(|k| CostCalculator::global().has_pricing(Some(k)))
        );
    }

    #[test]
    fn a_declined_attempt_with_output_bills_at_its_own_model() {
        let attempt = |model: Option<&str>, output| crate::domain::UsageIteration {
            model: model.map(str::to_string),
            input_tokens: 10,
            output_tokens: output,
            ..Default::default()
        };
        let mut u = usage(10, 0, 0, 7);
        u.iterations = vec![attempt(Some("claude-fable-5"), 5), attempt(None, 7)];
        let billed = billed_attempts("claude-opus-4-8", &u, None);
        let keys: Vec<_> = billed
            .iter()
            .map(|(k, a)| (k.as_str(), a.output_tokens))
            .collect();
        assert_eq!(keys, [("claude-opus-4-8", 7), ("claude-fable-5", 5)]);

        u.iterations[0].output_tokens = 0;
        assert_eq!(billed_attempts("claude-opus-4-8", &u, None).len(), 1);
        u.iterations.truncate(1);
        u.iterations[0].output_tokens = 7;
        assert_eq!(
            billed_attempts("claude-opus-4-8", &u, None).len(),
            1,
            "a lone record is the returned attempt itself"
        );
    }

    // A message at an unrated speed leaves its cost unknown, so its model
    // carries the unpriced mark even though the model has a row.
    #[test]
    fn an_unrated_speed_marks_its_model_unpriced() {
        let calc = CostCalculator::new();
        let mut tokens = TokenStats::default();
        tokens.add(&crate::domain::Usage {
            speed: Some("turbo".to_string()),
            ..usage(1, 0, 0, 1)
        });
        let model_tokens = HashMap::from([("claude-opus-4-6".to_string(), tokens)]);
        assert!(calc.has_pricing(Some("claude-opus-4-6")));
        assert!(
            calc.models_without_pricing(&model_tokens)
                .contains("Opus 4.6")
        );
    }

    #[test]
    fn opus_5_5_rates_match_the_published_table_and_stay_off_opus_5() {
        let calculator = CostCalculator::new();
        let p = calculator.get_pricing(Some("claude-opus-5-5")).unwrap();
        assert_eq!(p.input_cost_per_mtok, 4.0);
        assert_eq!(p.output_cost_per_mtok, 20.0);
        assert_eq!(p.cache_write_5m_cost_per_mtok, 5.0);
        assert_eq!(p.cache_write_1h_cost_per_mtok, 8.0);
        assert_eq!(p.cache_read_cost_per_mtok, 0.20);

        let long_ctx = calculator.get_pricing(Some("claude-opus-5-5[1m]")).unwrap();
        assert_eq!(long_ctx.input_cost_per_mtok, 4.0);
        assert_eq!(normalize_model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(normalize_model_name("claude-opus-5-5[1m]"), "Opus 5.5");

        let o5 = calculator.get_pricing(Some("claude-opus-5")).unwrap();
        assert_eq!(o5.input_cost_per_mtok, 5.0);
        assert_eq!(o5.cache_read_cost_per_mtok, 0.50);
        assert_eq!(normalize_model_name("claude-opus-5"), "Opus 5");
    }

    #[test]
    fn opus_5_rates_match_the_published_table() {
        // Comparing the two ids to each other passes for any rate, so pin the
        // absolute numbers: a mistyped digit reaches every cost surface with
        // no `$?` marker to hint at it.
        let calculator = CostCalculator::new();
        let pricing = calculator.get_pricing(Some("claude-opus-5")).unwrap();
        assert_eq!(pricing.input_cost_per_mtok, 5.0);
        assert_eq!(pricing.output_cost_per_mtok, 25.0);
        assert_eq!(pricing.cache_write_5m_cost_per_mtok, 6.25);
        assert_eq!(pricing.cache_write_1h_cost_per_mtok, 10.0);
        assert_eq!(pricing.cache_read_cost_per_mtok, 0.50);
    }

    #[test]
    fn models_without_pricing_flags_only_unpriced_entries() {
        let calculator = CostCalculator::new();
        let zero = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let mut model_tokens = std::collections::HashMap::new();
        model_tokens.insert("claude-sonnet-4".to_string(), zero.clone());
        model_tokens.insert("unknown-model-xyz".to_string(), zero.clone());
        model_tokens.insert("claude-mythos-preview".to_string(), zero);

        let unpriced = calculator.models_without_pricing(&model_tokens);
        assert!(!unpriced.contains(&normalize_model_name("claude-sonnet-4")));
        assert!(unpriced.contains(&normalize_model_name("unknown-model-xyz")));
        assert!(unpriced.contains(&normalize_model_name("claude-mythos-preview")));
        assert_eq!(unpriced.len(), 2);
    }

    #[test]
    fn models_without_pricing_empty_input_is_empty() {
        let calculator = CostCalculator::new();
        let model_tokens = std::collections::HashMap::new();
        assert!(calculator.models_without_pricing(&model_tokens).is_empty());
    }

    #[test]
    fn test_get_pricing_unknown_model() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(Some("unknown-model-xyz")).is_none());
    }

    #[test]
    fn test_get_pricing_context_window_suffix() {
        // Claude Code's 1M-context variant carries a `[1m]` suffix on the model
        // ID (e.g. Cowork audit.jsonl: `claude-opus-4-7[1m]`). Without explicit
        // boundary handling this would not match the base `claude-opus-4-7`
        // pricing entry and cost would silently fall through to $0.
        let calculator = CostCalculator::new();
        assert!(
            calculator
                .get_pricing(Some("claude-opus-4-7[1m]"))
                .is_some(),
            "[1m] context-window suffix should match the base opus-4-7 pricing"
        );
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-6[1m]"))
                .is_some(),
            "[1m] suffix should also work for other families"
        );
    }

    #[test]
    fn test_get_pricing_no_version_fallback() {
        let calculator = CostCalculator::new();
        // An undefined future version must NOT fall back to an older one.
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-6-20270101"))
                .is_none(),
            "undefined claude-sonnet-6 must not fall back to sonnet-5/4"
        );
        let s5 = calculator
            .get_pricing(Some("claude-sonnet-5-20260701"))
            .expect("sonnet-5 is priced");
        assert_eq!(s5.input_cost_per_mtok, 2.0);
        assert_eq!(s5.output_cost_per_mtok, 10.0);
        // "claude-sonnet-4-6" with date suffix should match
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-6-20260101"))
                .is_some()
        );
        // "claude-sonnet-4" with date suffix should still match
        assert!(
            calculator
                .get_pricing(Some("claude-sonnet-4-20250514"))
                .is_some()
        );
    }

    #[test]
    fn test_get_pricing_none() {
        let calculator = CostCalculator::new();
        assert!(calculator.get_pricing(None).is_none());
    }

    #[test]
    fn test_get_pricing_known_model() {
        let calculator = CostCalculator::new();
        let pricing = calculator
            .get_pricing(Some("claude-opus-4-5-20251101"))
            .unwrap();
        assert_eq!(pricing.input_cost_per_mtok, 5.0);
        assert_eq!(pricing.output_cost_per_mtok, 25.0);
    }

    #[test]
    fn test_calculate_cost_unknown_model_returns_none() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator.calculate_cost(&tokens, Some("unknown-model"));
        assert_eq!(cost, None);
    }

    #[test]
    fn test_calculate_cost_none_model_returns_none() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator.calculate_cost(&tokens, None);
        assert_eq!(cost, None);
    }

    #[test]
    fn test_calculate_cost_basic() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 100_000,
            output_tokens: 100_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // 100K input @ $3/M = $0.30, 100K output @ $15/M = $1.50 → $1.80
        assert!((cost - 1.80).abs() < 0.01);
    }

    #[test]
    fn test_simplify_model_name() {
        assert_eq!(
            CostCalculator::simplify_model_name("claude-opus-4-5-20251101"),
            "Opus 4.5"
        );
        assert_eq!(
            CostCalculator::simplify_model_name("claude-sonnet-4-20250514"),
            "Sonnet 4"
        );
        assert_eq!(
            CostCalculator::simplify_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
    }

    #[test]
    fn test_calculate_cost_large_token_count() {
        let calculator = CostCalculator::new();
        // Flat rate regardless of token count (no tiered pricing)
        let tokens = TokenStats {
            input_tokens: 500_000,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // 500K @ $3/M = $1.50
        assert!(
            (cost - 1.50).abs() < 0.01,
            "Expected ~$1.50, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_with_cache_tokens() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 100_000,
            output_tokens: 0,
            cache_creation_tokens: 50_000,
            cache_read_tokens: 25_000,
            // 50K all 5m TTL — matches Anthropic's API key default.
            cache_creation_5m_tokens: 50_000,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4"))
            .unwrap();
        // input: 100K @ $3/M = $0.30 (input_tokens is already non-cached per API spec)
        // cache_write (5m): 50K @ $3.75/M = $0.1875
        // cache_read: 25K @ $0.30/M = $0.0075
        // Total: ~$0.495
        assert!(
            (cost - 0.495).abs() < 0.01,
            "Expected ~$0.495, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_1h_cache_write_is_2x_base() {
        // Subscription users (Claude Pro/Max/Team via Claude Code) default
        // to 1h TTL. The 1h rate is 2x base input — `cache_write_1h` for
        // Sonnet 4.6 = $6/MTok (base $3 × 2).
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 1_000_000,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4-6"))
            .unwrap();
        assert!(
            (cost - 6.0).abs() < 0.01,
            "1h cache for Sonnet 4.6 should be $6/MTok, got ${cost:.4}"
        );
    }

    #[test]
    fn test_calculate_cost_mixed_5m_and_1h_cache_writes() {
        // Realistic Claude Code workload: most cache writes are 1h, a few
        // are 5m. The two buckets should be billed at their own rates and
        // summed independently — not merged at a single average rate.
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 1_100_000,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 100_000,
            cache_creation_1h_tokens: 1_000_000,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-sonnet-4-6"))
            .unwrap();
        // 5m: 100K @ $3.75/M = $0.375
        // 1h: 1M  @ $6.00/M = $6.000
        // Total: $6.375
        assert!(
            (cost - 6.375).abs() < 0.01,
            "mixed 5m+1h cache cost should be $6.375 for Sonnet 4.6, got ${cost:.4}"
        );
    }

    #[test]
    fn test_token_stats_add_routes_structured_cache_creation() {
        // Verify the aggregator splits 5m and 1h into separate fields when
        // the structured `cache_creation` object is present (modern JSONL).
        use crate::domain::{CacheCreationBreakdown, Usage};
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 30_000,
            cache_read_input_tokens: 0,
            cache_creation: Some(CacheCreationBreakdown {
                ephemeral_5m_input_tokens: 10_000,
                ephemeral_1h_input_tokens: 20_000,
            }),
            service_tier: None,
            speed: None,
            iterations: Vec::new(),
        };
        let mut stats = TokenStats::default();
        stats.add(&usage);
        assert_eq!(stats.cache_creation_5m_tokens, 10_000);
        assert_eq!(stats.cache_creation_1h_tokens, 20_000);
        assert_eq!(stats.cache_creation_tokens, 30_000);
    }

    #[test]
    fn test_token_stats_add_falls_back_to_flat_as_5m_when_no_structured() {
        // Legacy JSONL without the structured field: the entire flat
        // aggregate goes into the 5m bucket, matching Anthropic's API key
        // default TTL.
        use crate::domain::Usage;
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 0,
            cache_creation: None,
            service_tier: None,
            speed: None,
            iterations: Vec::new(),
        };
        let mut stats = TokenStats::default();
        stats.add(&usage);
        assert_eq!(stats.cache_creation_5m_tokens, 50_000);
        assert_eq!(stats.cache_creation_1h_tokens, 0);
        assert_eq!(stats.cache_creation_tokens, 50_000);
    }

    #[test]
    fn test_calculate_cost_opus() {
        let calculator = CostCalculator::new();
        let tokens = TokenStats {
            input_tokens: 500_000,
            output_tokens: 100_000,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_5m_tokens: 0,
            cache_creation_1h_tokens: 0,
            non_standard_speed: false,
        };
        let cost = calculator
            .calculate_cost(&tokens, Some("claude-opus-4-5"))
            .unwrap();
        // 500K @ $5/M = $2.50, 100K @ $25/M = $2.50 → total $5.00
        assert!(
            (cost - 5.00).abs() < 0.01,
            "Expected ~$5.00, got ${cost:.4}"
        );
    }

    #[test]
    fn test_simplify_model_name_unknown_keeps_raw() {
        // Unknown model families now keep their raw API name so the UI can list them
        // individually (and flag them as missing pricing). Avoids collapsing distinct
        // unknown models into a single opaque "Other" bucket.
        assert_eq!(
            CostCalculator::simplify_model_name("some-random-model"),
            "some-random-model"
        );
    }

    #[test]
    fn test_simplify_model_name_empty_and_placeholder() {
        // Empty / placeholder values still go to "Other" because they are not real models.
        assert_eq!(CostCalculator::simplify_model_name(""), "Other");
        assert_eq!(CostCalculator::simplify_model_name("<unknown>"), "Other");
    }

    #[test]
    fn test_normalize_model_name_future_versions() {
        assert_eq!(normalize_model_name("claude-opus-4-6-20260101"), "Opus 4.6");
        assert_eq!(normalize_model_name("claude-sonnet-5-20260101"), "Sonnet 5");
        assert_eq!(
            normalize_model_name("claude-haiku-5-1-20260101"),
            "Haiku 5.1"
        );
    }

    #[test]
    fn test_normalize_model_name_old_naming() {
        assert_eq!(
            normalize_model_name("claude-3-7-sonnet-20250219"),
            "Sonnet 3.7"
        );
        assert_eq!(normalize_model_name("claude-3-opus-20240229"), "Opus 3");
        assert_eq!(
            normalize_model_name("claude-3-5-haiku-20241022"),
            "Haiku 3.5"
        );
    }

    #[test]
    fn test_all_defined_models_have_pricing() {
        let calculator = CostCalculator::new();
        let expected_models = [
            "claude-fable-5-1",
            "claude-mythos-5-1",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-opus-4",
            "claude-sonnet-5-5",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-sonnet-4",
            "claude-3-7-sonnet",
            "claude-3-5-sonnet",
            "claude-haiku-4-5",
            "claude-3-5-haiku",
            "claude-3-haiku",
        ];
        for model in expected_models {
            assert!(
                calculator.pricing.contains_key(model),
                "Missing pricing for: {model}"
            );
        }
    }
}
