use brain_evolver::sandbox::Sandbox;
use std::path::Path;

fn setup_git_repo(dir: &std::path::Path) {
    for args in [
        &["init"][..],
        &["config", "user.email", "test@test.com"][..],
        &["config", "user.name", "test"][..],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    }
    // Create a file and commit
    std::fs::write(
        dir.join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .unwrap();
}

#[tokio::test]
async fn test_full_sandbox_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    setup_git_repo(tmp.path());

    let sandbox = Sandbox::create(tmp.path(), "lifecycle-test").await.unwrap();

    // 1. Read original code
    let original = sandbox.read_file(Path::new("lib.rs")).await.unwrap();
    assert!(original.contains("a + b"));

    // 2. Modify code
    sandbox
        .write_file(
            Path::new("lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a.wrapping_add(b) }",
        )
        .await
        .unwrap();

    // 3. Verify modification
    let modified = sandbox.read_file(Path::new("lib.rs")).await.unwrap();
    assert!(modified.contains("wrapping_add"));

    // 4. Diff
    let diff = sandbox.diff().await.unwrap();
    assert!(diff.contains("wrapping_add"));

    // 5. Discard
    sandbox.discard().await.unwrap();
    assert!(!sandbox.worktree_path.exists());
}

#[tokio::test]
async fn test_sandbox_isolation() {
    let tmp = tempfile::tempdir().unwrap();
    setup_git_repo(tmp.path());

    let original = std::fs::read_to_string(tmp.path().join("lib.rs")).unwrap();

    let sandbox = Sandbox::create(tmp.path(), "iso-test").await.unwrap();
    sandbox
        .write_file(Path::new("lib.rs"), "MODIFIED")
        .await
        .unwrap();

    // Main repo code is NOT affected
    let still_original = std::fs::read_to_string(tmp.path().join("lib.rs")).unwrap();
    assert_eq!(original, still_original);
    assert!(!still_original.contains("MODIFIED"));

    sandbox.discard().await.unwrap();
}

#[tokio::test]
async fn test_duplicate_sandbox_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    setup_git_repo(tmp.path());

    let _s1 = Sandbox::create(tmp.path(), "dup-test").await.unwrap();

    // Second sandbox with same ID should fail
    let result = Sandbox::create(tmp.path(), "dup-test").await;
    assert!(result.is_err());

    _s1.discard().await.unwrap();
}
