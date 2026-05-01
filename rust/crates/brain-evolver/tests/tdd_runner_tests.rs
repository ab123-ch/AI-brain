use std::sync::Arc;

use brain_evolver::sandbox::Sandbox;
use brain_evolver::{TddPhase, TddRunner};
use brain_llm::{ChatRequest, ChatResponse, LlmProvider};

// ---------------------------------------------------------------------------
// Mock LLM Provider (only needs to satisfy the trait; no real calls required)
// ---------------------------------------------------------------------------

struct MockLlmProvider;

impl LlmProvider for MockLlmProvider {
    fn model(&self) -> &str {
        "mock-model"
    }

    fn complete(
        &self,
        _request: ChatRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>>
    {
        Box::pin(async {
            Ok(ChatResponse {
                content: vec![brain_llm::ContentBlock::text("mock response")],
                model: "mock-model".into(),
                usage: brain_llm::TokenUsage::default(),
                finish_reason: Some(brain_llm::FinishReason::EndTurn),
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal git repo with an initial commit so worktree add works.
fn setup_git_repo(dir: &std::path::Path) {
    for args in [
        &["init"][..],
        &["config", "user.email", "test@test.com"][..],
        &["config", "user.name", "test"][..],
        &["commit", "--allow-empty", "-m", "init"][..],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    }
}

/// Create a TddRunner backed by a real Sandbox in a temp git repo.
///
/// Returns `(TddRunner, tempdir)` -- the caller **must** keep `tempdir` alive
/// for the duration of the test because the sandbox worktree lives inside it.
async fn create_runner(max_iterations: u32) -> (TddRunner, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("create temp dir");
    setup_git_repo(dir.path());

    let evolution_id = format!("test-{}", std::process::id());
    let sandbox = Sandbox::create(dir.path(), &evolution_id)
        .await
        .expect("create sandbox");

    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlmProvider);
    let runner = TddRunner::new(sandbox, llm, max_iterations);

    (runner, dir)
}

// ---------------------------------------------------------------------------
// Tests: phase transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initial_phase_is_analyzing() {
    let (runner, _dir) = create_runner(3).await;
    assert_eq!(runner.phase(), &TddPhase::Analyzing);
}

#[tokio::test]
async fn advance_to_writing_tests() {
    let (mut runner, _dir) = create_runner(3).await;
    runner.advance(TddPhase::WritingTests);
    assert_eq!(runner.phase(), &TddPhase::WritingTests);
}

#[tokio::test]
async fn advance_to_red_verification() {
    let (mut runner, _dir) = create_runner(3).await;
    runner.advance(TddPhase::WritingTests);
    runner.advance(TddPhase::RedVerification);
    assert_eq!(runner.phase(), &TddPhase::RedVerification);
}

#[tokio::test]
async fn advance_full_happy_path() {
    let (mut runner, _dir) = create_runner(3).await;

    let phases = [
        TddPhase::WritingTests,
        TddPhase::RedVerification,
        TddPhase::Implementing,
        TddPhase::GreenCheck,
        TddPhase::Regression,
        TddPhase::Reporting,
        TddPhase::Done,
    ];

    for phase in phases {
        runner.advance(phase.clone());
        assert_eq!(runner.phase(), &phase);
    }
}

#[tokio::test]
async fn advance_to_failed() {
    let (mut runner, _dir) = create_runner(3).await;
    runner.advance(TddPhase::Failed);
    assert_eq!(runner.phase(), &TddPhase::Failed);
}

// ---------------------------------------------------------------------------
// Tests: iteration counting
// ---------------------------------------------------------------------------

#[tokio::test]
async fn iterations_used_starts_at_zero() {
    let (runner, _dir) = create_runner(5).await;
    assert_eq!(runner.iterations_used(), 0);
}

#[tokio::test]
async fn advancing_to_implementing_increments_iterations() {
    let (mut runner, _dir) = create_runner(5).await;

    runner.advance(TddPhase::WritingTests);
    assert_eq!(runner.iterations_used(), 0);

    runner.advance(TddPhase::RedVerification);
    assert_eq!(runner.iterations_used(), 0);

    // First time hitting Implementing -> iterations_used = 1
    runner.advance(TddPhase::Implementing);
    assert_eq!(runner.iterations_used(), 1);
}

#[tokio::test]
async fn each_implementing_phase_increments_iterations() {
    let (mut runner, _dir) = create_runner(5).await;

    // Simulate: cycle 1
    runner.advance(TddPhase::Implementing);
    assert_eq!(runner.iterations_used(), 1);

    runner.advance(TddPhase::GreenCheck);
    assert_eq!(runner.iterations_used(), 1); // not Implementing, no increment

    // Cycle back to Implementing for second iteration
    runner.advance(TddPhase::Implementing);
    assert_eq!(runner.iterations_used(), 2);

    // Third iteration
    runner.advance(TddPhase::GreenCheck);
    runner.advance(TddPhase::Implementing);
    assert_eq!(runner.iterations_used(), 3);
}

#[tokio::test]
async fn non_implementing_phases_do_not_increment() {
    let (mut runner, _dir) = create_runner(5).await;

    for phase in [
        TddPhase::WritingTests,
        TddPhase::RedVerification,
        TddPhase::GreenCheck,
        TddPhase::Regression,
        TddPhase::Reporting,
        TddPhase::Done,
    ] {
        runner.advance(phase);
    }
    assert_eq!(runner.iterations_used(), 0);
}

// ---------------------------------------------------------------------------
// Tests: is_exhausted
// ---------------------------------------------------------------------------

#[tokio::test]
async fn is_exhausted_false_when_below_max() {
    let (mut runner, _dir) = create_runner(5).await;
    runner.advance(TddPhase::Implementing);
    assert!(!runner.is_exhausted()); // 1 < 5
}

#[tokio::test]
async fn is_exhausted_true_when_at_max() {
    let (mut runner, _dir) = create_runner(3).await;

    // Burn through 3 Implementing phases
    for _ in 0..3 {
        runner.advance(TddPhase::Implementing);
    }
    assert!(runner.is_exhausted()); // 3 >= 3
}

#[tokio::test]
async fn is_exhausted_true_when_exceeding_max() {
    let (mut runner, _dir) = create_runner(2).await;

    runner.advance(TddPhase::Implementing);
    runner.advance(TddPhase::Implementing);
    runner.advance(TddPhase::Implementing); // 3 > 2
    assert!(runner.is_exhausted());
}

#[tokio::test]
async fn is_exhausted_with_max_one() {
    let (mut runner, _dir) = create_runner(1).await;
    assert!(!runner.is_exhausted()); // 0 < 1

    runner.advance(TddPhase::Implementing);
    assert!(runner.is_exhausted()); // 1 >= 1
}

#[tokio::test]
async fn is_exhausted_with_max_zero() {
    let (runner, _dir) = create_runner(0).await;
    // Even before any advance, iterations_used (0) >= max_iterations (0)
    assert!(runner.is_exhausted());
}

// ---------------------------------------------------------------------------
// Tests: phase transition with exhaustion check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_tdd_cycle_within_budget() {
    let (mut runner, _dir) = create_runner(5).await;

    // Cycle 1: full TDD round-trip
    runner.advance(TddPhase::WritingTests);
    runner.advance(TddPhase::RedVerification);
    runner.advance(TddPhase::Implementing); // iter 1
    runner.advance(TddPhase::GreenCheck);
    runner.advance(TddPhase::Regression);
    assert!(!runner.is_exhausted());

    // Cycle 2: another round
    runner.advance(TddPhase::WritingTests);
    runner.advance(TddPhase::RedVerification);
    runner.advance(TddPhase::Implementing); // iter 2
    runner.advance(TddPhase::GreenCheck);
    runner.advance(TddPhase::Regression);
    assert!(!runner.is_exhausted());

    // Finish
    runner.advance(TddPhase::Reporting);
    runner.advance(TddPhase::Done);
    assert_eq!(runner.iterations_used(), 2);
    assert_eq!(runner.phase(), &TddPhase::Done);
    assert!(!runner.is_exhausted());
}

#[tokio::test]
async fn exhaustion_stops_tdd_cycle() {
    let (mut runner, _dir) = create_runner(2).await;

    // Cycle 1
    runner.advance(TddPhase::Implementing); // iter 1
    assert!(!runner.is_exhausted());

    // Cycle 2
    runner.advance(TddPhase::Implementing); // iter 2
    assert!(runner.is_exhausted());

    // Should transition to Failed instead of continuing
    runner.advance(TddPhase::Failed);
    assert_eq!(runner.phase(), &TddPhase::Failed);
}

// ---------------------------------------------------------------------------
// Tests: phase equality & debug
// ---------------------------------------------------------------------------

#[test]
fn tdd_phase_equality() {
    assert_eq!(TddPhase::Analyzing, TddPhase::Analyzing);
    assert_ne!(TddPhase::Analyzing, TddPhase::Done);
    assert_ne!(TddPhase::Implementing, TddPhase::GreenCheck);
}

#[test]
fn tdd_phase_debug_format() {
    let phase = TddPhase::RedVerification;
    let debug_str = format!("{:?}", phase);
    assert!(debug_str.contains("RedVerification"));
}
