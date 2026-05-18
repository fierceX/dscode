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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelBelief {
    pub name: String,
    pub alpha: f64, // success + 1 (uniform prior)
    pub beta: f64,  // failure + 1 (uniform prior)
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
    /// When means are equal, prefers "flash" (the cheaper model) as default.
    /// If no models are registered, returns "flash".
    pub fn select_greedy(&self) -> &str {
        self.beliefs
            .iter()
            .max_by(|a, b| {
                a.mean().partial_cmp(&b.mean())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    // Equal means → prefer "flash" (cheaper default)
                    .then_with(|| {
                        if a.name == "flash" { std::cmp::Ordering::Greater }
                        else if b.name == "flash" { std::cmp::Ordering::Less }
                        else { std::cmp::Ordering::Equal }
                    })
            })
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

    /// Total observations for a model (α + β - 2, excluding the prior).
    pub fn observations(&self, model: &str) -> u64 {
        self.beliefs
            .iter()
            .find(|b| b.name == model)
            .map(|b| (b.alpha + b.beta - 2.0) as u64)
            .unwrap_or(0)
    }

    /// Reset a model's beliefs to a specific Beta(α, β) prior.
    /// Creates the model if it doesn't exist.
    pub fn reset_belief(&mut self, model: &str, alpha: f64, beta: f64) {
        if let Some(b) = self.beliefs.iter_mut().find(|b| b.name == model) {
            b.alpha = alpha;
            b.beta = beta;
        } else {
            self.beliefs.push(ModelBelief {
                name: model.to_string(),
                alpha,
                beta,
            });
        }
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

    /// Save beliefs to a JSON file for session persistence.
    /// Returns Ok(true) if written, Ok(false) if no beliefs to save.
    pub fn save_to_path(&self, path: &std::path::Path) -> anyhow::Result<bool> {
        if self.beliefs.is_empty() {
            return Ok(false);
        }
        let json = serde_json::to_string(&self.beliefs)?;
        std::fs::write(path, json)?;
        Ok(true)
    }

    /// Load beliefs from a JSON file, replacing current beliefs.
    /// If the file doesn't exist or is unreadable, keeps current state.
    pub fn load_from_path(&mut self, path: &std::path::Path) -> anyhow::Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let data = std::fs::read_to_string(path)?;
        let beliefs: Vec<ModelBelief> = serde_json::from_str(&data)?;
        self.beliefs = beliefs;
        Ok(true)
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
    fn save_and_load_roundtrip_preserves_beliefs() {
        let mut ms = ModelSelector::new();
        ms.ensure("flash");
        ms.ensure("pro");
        ms.update("pro", true);
        ms.update("pro", true);
        ms.update("flash", false);
        let pro_mean = ms.mean("pro");
        let flash_mean = ms.mean("flash");

        let dir = std::env::temp_dir().join(format!("model-beliefs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("model_beliefs.json");

        assert!(ms.save_to_path(&path).unwrap());
        let mut ms2 = ModelSelector::new();
        assert!(ms2.load_from_path(&path).unwrap());
        assert!((ms2.mean("pro") - pro_mean).abs() < 1e-10);
        assert!((ms2.mean("flash") - flash_mean).abs() < 1e-10);
        assert_eq!(ms2.select_greedy(), ms.select_greedy());
    }

    #[test]
    fn load_from_nonexistent_file_returns_false() {
        let mut ms = ModelSelector::new();
        let path = std::path::Path::new("/tmp/__nonexistent_model_beliefs_xyz.json");
        assert!(!ms.load_from_path(path).unwrap());
    }

    #[test]
    fn save_empty_beliefs_returns_false() {
        let ms = ModelSelector::new();
        let dir = std::env::temp_dir().join(format!("empty-beliefs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("empty.json");
        assert!(!ms.save_to_path(&path).unwrap());
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
