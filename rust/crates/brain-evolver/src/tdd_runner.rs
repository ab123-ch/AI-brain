use crate::sandbox::Sandbox;
use brain_llm::LlmProvider;
use std::sync::Arc;

/// TDD 阶段
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TddPhase {
    Analyzing,
    WritingTests,
    RedVerification,
    Implementing,
    GreenCheck,
    Regression,
    Reporting,
    Failed,
    Done,
}

/// 进化目标
#[derive(Debug, Clone)]
pub struct EvolutionGoal {
    pub description: String,
    pub target_files: Vec<std::path::PathBuf>,
    pub expected_outcome: String,
    pub test_scenarios: Vec<String>,
}

/// TDD 测试结果
#[derive(Debug)]
pub struct TestResult {
    pub success: bool,
    pub passed: usize,
    pub failed: usize,
    pub output: String,
}

/// Lint 检查结果
#[derive(Debug)]
pub struct LintResult {
    pub clippy_passed: bool,
    pub fmt_passed: bool,
    pub clippy_output: String,
    pub fmt_output: String,
}

/// TDD 流程控制器
pub struct TddRunner {
    sandbox: Sandbox,
    #[allow(dead_code)] // 后续 LLM 驱动测试生成使用
    llm: Arc<dyn LlmProvider>,
    max_iterations: u32,
    phase: TddPhase,
    iterations_used: u32,
}

impl TddRunner {
    pub fn new(sandbox: Sandbox, llm: Arc<dyn LlmProvider>, max_iterations: u32) -> Self {
        Self {
            sandbox,
            llm,
            max_iterations,
            phase: TddPhase::Analyzing,
            iterations_used: 0,
        }
    }

    pub fn phase(&self) -> &TddPhase {
        &self.phase
    }

    pub fn iterations_used(&self) -> u32 {
        self.iterations_used
    }

    /// 运行 cargo test 并收集结果
    pub async fn run_tests(&self) -> crate::error::Result<TestResult> {
        let result = self
            .sandbox
            .exec("cargo test --workspace --no-fail-fast 2>&1")
            .await?;
        let passed = result
            .stdout
            .lines()
            .filter(|l| l.contains("... ok"))
            .count();
        let failed = result
            .stdout
            .lines()
            .filter(|l| l.contains("... FAILED"))
            .count();
        Ok(TestResult {
            success: result.success,
            passed,
            failed,
            output: result.stdout.clone(),
        })
    }

    /// 运行 cargo clippy + fmt check
    pub async fn lint_check(&self) -> crate::error::Result<LintResult> {
        let clippy = self
            .sandbox
            .exec("cargo clippy --workspace --all-targets -- -D warnings 2>&1")
            .await?;
        let fmt = self.sandbox.exec("cargo fmt --check 2>&1").await?;
        Ok(LintResult {
            clippy_passed: clippy.success,
            fmt_passed: fmt.success,
            clippy_output: clippy.stdout,
            fmt_output: fmt.stdout,
        })
    }

    /// 全量回归测试
    pub async fn regression_test(&self) -> crate::error::Result<TestResult> {
        self.run_tests().await
    }

    /// 推进到下一阶段
    pub fn advance(&mut self, next: TddPhase) {
        if next == TddPhase::Implementing {
            self.iterations_used += 1;
        }
        self.phase = next;
    }

    /// 检查是否超过最大迭代次数
    pub fn is_exhausted(&self) -> bool {
        self.iterations_used >= self.max_iterations
    }
}
