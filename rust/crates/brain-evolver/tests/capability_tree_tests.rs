use brain_evolver::capability_tree::*;
use tempfile::TempDir;

#[test]
fn test_save_and_load() {
    let tmp = TempDir::new().unwrap();

    let tree = CapabilityTree {
        last_updated: "2026-06-08T12:00:00Z".to_string(),
        domains: vec![SkillDomain {
            name: "Rust".to_string(),
            skills: vec!["async programming".to_string()],
            coverage: "minimal".to_string(),
        }],
        gaps: vec![],
    };
    tree.save(tmp.path()).unwrap();

    let loaded = CapabilityTree::load(tmp.path()).unwrap();
    assert_eq!(loaded.domains.len(), 1);
    assert_eq!(loaded.domains[0].name, "Rust");
    assert_eq!(loaded.domains[0].skills, vec!["async programming"]);
}

#[test]
fn test_update_with_new_skill_existing_domain() {
    let tmp = TempDir::new().unwrap();
    let mut tree = CapabilityTree::load(tmp.path()).unwrap();

    // Add first skill to "Rust" domain.
    tree.update_with_new_skill("async programming", Some("Rust"));
    // Add second skill to same domain.
    tree.update_with_new_skill("trait design", Some("Rust"));

    assert_eq!(tree.domains.len(), 1);
    assert_eq!(tree.domains[0].skills.len(), 2);
    assert!(tree.domains[0]
        .skills
        .contains(&"async programming".to_string()));
    assert!(tree.domains[0].skills.contains(&"trait design".to_string()));
    // 2 skills -> "minimal" coverage, which creates a gap entry.
    assert_eq!(tree.domains[0].coverage, "minimal");
    assert_eq!(tree.gaps.len(), 1);
    assert_eq!(tree.gaps[0].domain, "Rust");

    // Duplicate skill should not be added.
    tree.update_with_new_skill("async programming", Some("Rust"));
    assert_eq!(tree.domains[0].skills.len(), 2);
}

#[test]
fn test_update_with_new_skill_new_domain() {
    let tmp = TempDir::new().unwrap();
    let mut tree = CapabilityTree::load(tmp.path()).unwrap();

    // No domain hint -> goes to "General".
    tree.update_with_new_skill("reading files", None);

    assert_eq!(tree.domains.len(), 1);
    assert_eq!(tree.domains[0].name, "General");
    assert_eq!(tree.domains[0].skills, vec!["reading files"]);

    // Explicit new domain.
    tree.update_with_new_skill("HTTP client", Some("Networking"));
    assert_eq!(tree.domains.len(), 2);

    // 3 skills in "General" would mean we need more entries to reach "partial":
    tree.update_with_new_skill("writing files", None);
    tree.update_with_new_skill("path handling", None);
    // "General" now has 3 skills -> "partial" coverage.
    let general = tree.domains.iter().find(|d| d.name == "General").unwrap();
    assert_eq!(general.coverage, "partial");
}
