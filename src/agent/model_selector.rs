/// Beta-Bernoulli model selector using Thompson Sampling (Greedy variant).
///
/// Each model maintains a Beta(α, β) belief representing its probability of
/// completing a turn successfully. The Greedy selector picks the model with
/// the highest mean success rate α/(α+β), which balances exploration and
/// exploitation without needing random sampling.
///
/// # Upgrade path
/// - Current: Greedy (zero dependencies, simple reliable)
/// - Future: Full Thompson Sampling with Beta-distribution random samples
///   (requires Gamma random number generation)
#[derive(Debug, Clone)]
pub struct ModelSelector {
    beliefs: Vec<ModelBelief>,
}

/// Belief state for a single model.
#[derive(Debug, Clone)]
struct ModelBelief {
    name: String,
    alpha: f64, // success + 1 (uniform prior)
    beta: f64,  // failure + 1 (uniform prior)
}

impl ModelBelief {
    /// Mean success rate: α/(α+β)
    fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

impl ModelSelector {
    /// Create a new empty model selector.
    pub fn new() -> Self {
        Self { beliefs: Vec::new() }
    }

    /// Ensure a model is registered with default prior Beta(1,1).
    /// If already registered, does nothing.
    pub fn ensure(&mut self, name: &str) {
        if !self.beliefs.iter().any(|b| b.name == name) {
            self.beliefs.push(ModelBelief {
                name: name.to_string(),
                alpha: 1.0,
                beta: 1.0,
            });
        }
    }

    /// Greedy selection: pick the model with the highest mean success rate.
    ///
    /// Returns the model name. If no models are registered, returns "flash".
    pub fn select_greedy(&self) -> &str {
        self.beliefs
            .iter()
            .max_by(|a, b| a.mean().partial_cmp(&b.mean()).unwrap_or(std::cmp::Ordering::Equal))
            .map(|b| b.name.as_str())
            .unwrap_or("flash")
    }

    /// Update belief after observing success (true) or failure (false).
    ///
    /// If the model is not registered, it is automatically added with default
    /// prior before updating.
    pub fn update(&mut self, model: &str, success: bool) {
        self.ensure(model);
        if let Some(b) = self.beliefs.iter_mut().find(|b| b.name == model) {
            if success {
                b.alpha += 1.0;
            } else {
                b.beta += 1.0;
            }
        }
    }

    /// Get the mean success rate for a specific model.
    /// Returns 0.5 (uniform prior) if model is not registered.
    pub fn mean(&self, model: &str) -> f64 {
        self.beliefs
            .iter()
            .find(|b| b.name == model)
            .map(|b| b.mean())
            .unwrap_or(0.5)
    }

    /// Get the number of registered models.
    pub fn len(&self) -> usize {
        self.beliefs.len()
    }

    /// Check if any models are registered.
    pub fn is_empty(&self) -> bool {
        self.beliefs.is_empty()
    }

    /// Format the current beliefs for logging.
    pub fn format_beliefs(&self) -> String {
        if self.beliefs.is_empty() {
            return "no models registered".to_string();
        }
        let parts: Vec<String> = self
            .beliefs
            .iter()
            .map(|b| format!("{}: α={:.0},β={:.0},mean={:.3}", b.name, b.alpha, b.beta, b.mean()))
            .collect();
        parts.join(", ")
    }

    /// Return structured JSON snapshot of model beliefs for logging.
    pub fn snapshot_beliefs(&self) -> serde_json::Value {
        let beliefs: Vec<serde_json::Value> = self.beliefs.iter().map(|b| {
            serde_json::json!({
                "model": b.name,
                "alpha": b.alpha,
                "beta": b.beta,
                "mean": b.mean(),
            })
        }).collect();
        serde_json::json!({
            "models": beliefs,
            "greedy_choice": self.select_greedy(),
        })
    }
}

impl Default for ModelSelector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_selector_initial_mean_is_05() {
        let mut ms = ModelSelector::new();
        ms.ensure("flash");
        let p = ms.mean("flash");
        assert!((p - 0.5).abs() < 1e-10, "expected 0.5, got {p}");
    }

    #[test]
    fn update_increases_alpha_on_success() {
        let mut ms = ModelSelector::new();
        ms.update("flash", true);
        let p = ms.mean("flash");
        assert!(p > 0.5, "expected >0.5, got {p}");
    }

    #[test]
    fn update_increases_beta_on_failure() {
        let mut ms = ModelSelector::new();
        ms.update("flash", false);
        let p = ms.mean("flash");
        assert!(p < 0.5, "expected <0.5, got {p}");
    }

    #[test]
    fn greedy_picks_better_model() {
        let mut ms = ModelSelector::new();
        ms.ensure("flash");
        ms.ensure("pro");
        // Make pro look very good
        for _ in 0..10 {
            ms.update("pro", true);
        }
        // Make flash look bad
        for _ in 0..5 {
            ms.update("flash", false);
        }
        assert_eq!(ms.select_greedy(), "pro");
    }

    #[test]
    fn selector_does_not_crash_with_empty_registry() {
        let ms = ModelSelector::new();
        assert_eq!(ms.select_greedy(), "flash");
    }

    #[test]
    fn ensure_does_not_duplicate() {
        let mut ms = ModelSelector::new();
        ms.ensure("flash");
        ms.ensure("flash");
        assert_eq!(ms.len(), 1);
    }

    #[test]
    fn update_auto_registers_model() {
        let mut ms = ModelSelector::new();
        ms.update("unknown-model", true);
        assert_eq!(ms.len(), 1);
        assert!(!ms.is_empty());
    }

    #[test]
    fn format_beliefs_contains_model_names() {
        let mut ms = ModelSelector::new();
        ms.ensure("flash");
        ms.ensure("pro");
        let s = ms.format_beliefs();
        assert!(s.contains("flash"));
        assert!(s.contains("pro"));
        assert!(s.contains("mean="));
    }

    #[test]
    fn multiple_successes_converge_to_one() {
        let mut ms = ModelSelector::new();
        ms.ensure("pro");
        for _ in 0..100 {
            ms.update("pro", true);
        }
        let p = ms.mean("pro");
        assert!(p > 0.99, "expected >0.99, got {p}");
    }
}
