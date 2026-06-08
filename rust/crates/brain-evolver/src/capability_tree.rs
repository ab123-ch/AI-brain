use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// A skill domain grouping related skills.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SkillDomain {
    pub name: String,
    pub skills: Vec<String>,
    pub coverage: String,
}

/// A gap in a specific domain — skills that are missing.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SkillGap {
    pub domain: String,
    pub missing: Vec<String>,
}

/// The full capability tree: domains, their skills, and identified gaps.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CapabilityTree {
    pub last_updated: String,
    pub domains: Vec<SkillDomain>,
    pub gaps: Vec<SkillGap>,
}

impl CapabilityTree {
    const FILENAME: &'static str = "capability_tree.json";

    /// Load from `base_dir/capability_tree.json`, or return an empty tree.
    pub fn load(base_dir: &Path) -> Result<Self, String> {
        let path = base_dir.join(Self::FILENAME);
        if path.exists() {
            let json = fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
            let tree: CapabilityTree =
                serde_json::from_str(&json).map_err(|e| format!("parse error: {e}"))?;
            Ok(tree)
        } else {
            Ok(Self {
                last_updated: Utc::now().to_rfc3339(),
                domains: Vec::new(),
                gaps: Vec::new(),
            })
        }
    }

    /// Persist to `base_dir/capability_tree.json`.
    pub fn save(&self, base_dir: &Path) -> Result<(), String> {
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize error: {e}"))?;
        fs::create_dir_all(base_dir).map_err(|e| format!("create dir error: {e}"))?;
        fs::write(base_dir.join(Self::FILENAME), json).map_err(|e| format!("write error: {e}"))
    }

    /// Incrementally add a skill. If `domain_hint` matches an existing domain name
    /// (case-insensitive), the skill is added there. Otherwise it goes into a
    /// "General" domain (created on demand). After mutation, gaps are recomputed.
    pub fn update_with_new_skill(&mut self, skill_name: &str, domain_hint: Option<&str>) {
        let target_domain = domain_hint
            .map(|h| h.to_string())
            .unwrap_or_else(|| "General".to_string());

        // Find matching domain (case-insensitive).
        let idx = self
            .domains
            .iter()
            .position(|d| d.name.eq_ignore_ascii_case(&target_domain));

        if let Some(i) = idx {
            let domain = &mut self.domains[i];
            if !domain
                .skills
                .iter()
                .any(|s| s.eq_ignore_ascii_case(skill_name))
            {
                domain.skills.push(skill_name.to_string());
            }
        } else {
            // Create a new domain with this skill.
            self.domains.push(SkillDomain {
                name: target_domain,
                skills: vec![skill_name.to_string()],
                coverage: "minimal".to_string(),
            });
        }

        self.recompute_gaps();
        self.last_updated = Utc::now().to_rfc3339();
    }

    /// Recompute coverage strings and the gap list.
    ///
    /// Coverage heuristic:
    ///   - 0 skills -> "none"
    ///   - 1-2      -> "minimal"
    ///   - 3-5      -> "partial"
    ///   - 6+       -> "good"
    fn recompute_gaps(&mut self) {
        // Update coverage based on skill count.
        for domain in &mut self.domains {
            domain.coverage = match domain.skills.len() {
                0 => "none".to_string(),
                1..=2 => "minimal".to_string(),
                3..=5 => "partial".to_string(),
                _ => "good".to_string(),
            };
        }

        // For now, domains with "none" or "minimal" coverage are gaps.
        // This is a simple heuristic; richer gap analysis can be added later.
        self.gaps = self
            .domains
            .iter()
            .filter(|d| d.coverage == "none" || d.coverage == "minimal")
            .map(|d| SkillGap {
                domain: d.name.clone(),
                missing: vec![format!("more skills needed (current: {})", d.skills.len())],
            })
            .collect();
    }
}
