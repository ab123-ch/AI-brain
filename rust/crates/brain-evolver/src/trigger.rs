use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Trigger check result indicating why evolution should or should not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDecision {
    /// All conditions met — evolution should begin.
    ShouldTrigger,
    /// Main brain has not been idle long enough.
    NotYetIdle,
    /// Current hour is outside the configured night-time window.
    NotNightTime,
    /// An evolution session is already in progress.
    AlreadyEvolving,
    /// No pending targets are available for evolution.
    NoTargets,
}

/// Configuration for the evolution trigger.
pub struct TriggerConfig {
    /// How long the main brain must be idle before triggering.
    pub idle_threshold: Duration,
    /// Start of the night-time window (inclusive, 24-hour clock).
    pub start_hour: u32,
    /// End of the night-time window (exclusive, 24-hour clock).
    pub end_hour: u32,
    /// Whether there are pending evolution targets (set externally).
    pub has_pending_targets: bool,
    /// Optional override for the current hour. `None` uses `chrono::Local::now().hour()`.
    pub current_hour_fn: Option<Box<dyn Fn() -> u32 + Send + Sync>>,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            idle_threshold: Duration::from_secs(3600), // 1 hour
            start_hour: 0,                             // midnight
            end_hour: 5,                               // before 5 AM
            has_pending_targets: false,
            current_hour_fn: None,
        }
    }
}

impl TriggerConfig {
    /// Returns the current hour, using the injected closure if set, or the real local time.
    pub fn current_hour(&self) -> u32 {
        match &self.current_hour_fn {
            Some(f) => f(),
            None => chrono::Local::now()
                .format("%H")
                .to_string()
                .parse::<u32>()
                .unwrap_or(0),
        }
    }

    /// Returns true if the current hour falls within `[start_hour, end_hour)`.
    ///
    /// Handles overnight ranges where `start_hour > end_hour` (e.g. 22..05).
    pub fn is_night_time(&self) -> bool {
        let hour = self.current_hour();
        if self.start_hour <= self.end_hour {
            hour >= self.start_hour && hour < self.end_hour
        } else {
            // Overnight window, e.g. 22..05
            hour >= self.start_hour || hour < self.end_hour
        }
    }
}

/// Lightweight trigger detector that checks whether conditions are right for
/// starting a self-evolution session.
pub struct EvolutionTrigger {
    config: TriggerConfig,
    last_activity: Arc<Mutex<Instant>>,
    is_evolving: Arc<AtomicBool>,
}

impl EvolutionTrigger {
    /// Create a new trigger with the given configuration.
    pub fn new(config: TriggerConfig) -> Self {
        Self {
            config,
            last_activity: Arc::new(Mutex::new(Instant::now())),
            is_evolving: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Evaluate all conditions and return the detailed decision.
    pub fn should_trigger(&self) -> TriggerDecision {
        if self.is_evolving() {
            return TriggerDecision::AlreadyEvolving;
        }
        if !self.is_idle() {
            return TriggerDecision::NotYetIdle;
        }
        if !self.config.is_night_time() {
            return TriggerDecision::NotNightTime;
        }
        if !self.config.has_pending_targets {
            return TriggerDecision::NoTargets;
        }
        TriggerDecision::ShouldTrigger
    }

    /// Record that the main brain just performed activity, resetting the idle timer.
    pub fn touch_activity(&self) {
        if let Ok(mut instant) = self.last_activity.lock() {
            *instant = Instant::now();
        }
    }

    /// Set whether an evolution session is currently in progress.
    pub fn set_evolving(&self, v: bool) {
        self.is_evolving.store(v, Ordering::SeqCst);
    }

    /// Set whether pending evolution targets exist.
    pub fn set_has_targets(&mut self, v: bool) {
        self.config.has_pending_targets = v;
    }

    /// Returns how long since the last recorded activity.
    pub fn idle_duration(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|instant| instant.elapsed())
            .unwrap_or(Duration::MAX)
    }

    /// Returns true if the main brain has been idle for at least `idle_threshold`.
    fn is_idle(&self) -> bool {
        self.idle_duration() >= self.config.idle_threshold
    }

    /// Returns true if an evolution session is currently in progress.
    pub fn is_evolving(&self) -> bool {
        self.is_evolving.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// Helper to build a config that simulates night-time (hour = 2).
    fn night_config() -> TriggerConfig {
        TriggerConfig {
            idle_threshold: Duration::from_millis(50),
            start_hour: 0,
            end_hour: 5,
            has_pending_targets: true,
            current_hour_fn: Some(Box::new(|| 2)),
        }
    }

    /// Helper to build a config that simulates day-time (hour = 14).
    fn day_config() -> TriggerConfig {
        TriggerConfig {
            idle_threshold: Duration::from_millis(50),
            start_hour: 0,
            end_hour: 5,
            has_pending_targets: true,
            current_hour_fn: Some(Box::new(|| 14)),
        }
    }

    #[test]
    fn test_should_trigger_all_conditions_met() {
        let config = night_config();
        let trigger = EvolutionTrigger::new(config);
        // Wait until idle threshold has passed.
        thread::sleep(Duration::from_millis(80));
        assert_eq!(trigger.should_trigger(), TriggerDecision::ShouldTrigger);
    }

    #[test]
    fn test_not_trigger_when_not_idle() {
        let config = night_config();
        let trigger = EvolutionTrigger::new(config);
        // Freshly created — should not be idle yet.
        trigger.touch_activity();
        assert_eq!(trigger.should_trigger(), TriggerDecision::NotYetIdle);
    }

    #[test]
    fn test_not_trigger_during_daytime() {
        let config = day_config();
        let trigger = EvolutionTrigger::new(config);
        thread::sleep(Duration::from_millis(80));
        assert_eq!(trigger.should_trigger(), TriggerDecision::NotNightTime);
    }

    #[test]
    fn test_not_trigger_when_already_evolving() {
        let config = night_config();
        let trigger = EvolutionTrigger::new(config);
        trigger.set_evolving(true);
        thread::sleep(Duration::from_millis(80));
        assert_eq!(trigger.should_trigger(), TriggerDecision::AlreadyEvolving);
    }

    #[test]
    fn test_not_trigger_when_no_targets() {
        let mut config = night_config();
        config.has_pending_targets = false;
        let trigger = EvolutionTrigger::new(config);
        thread::sleep(Duration::from_millis(80));
        assert_eq!(trigger.should_trigger(), TriggerDecision::NoTargets);
    }

    #[test]
    fn test_touch_activity_resets_idle() {
        let config = night_config();
        let threshold = config.idle_threshold;
        let trigger = EvolutionTrigger::new(config);
        // Wait to become idle.
        thread::sleep(Duration::from_millis(80));
        assert!(trigger.idle_duration() >= threshold);
        // Touch resets the timer.
        trigger.touch_activity();
        assert!(trigger.idle_duration() < threshold);
        assert_eq!(trigger.should_trigger(), TriggerDecision::NotYetIdle);
    }

    #[test]
    fn test_overnight_window_crosses_midnight() {
        // Window 22..05 should include hour 23 and hour 3.
        let config_inside = TriggerConfig {
            start_hour: 22,
            end_hour: 5,
            current_hour_fn: Some(Box::new(|| 23)),
            ..TriggerConfig::default()
        };
        assert!(config_inside.is_night_time());

        let config_inside2 = TriggerConfig {
            start_hour: 22,
            end_hour: 5,
            current_hour_fn: Some(Box::new(|| 3)),
            ..TriggerConfig::default()
        };
        assert!(config_inside2.is_night_time());

        // Hour 12 should be outside 22..05.
        let config_outside = TriggerConfig {
            start_hour: 22,
            end_hour: 5,
            current_hour_fn: Some(Box::new(|| 12)),
            ..TriggerConfig::default()
        };
        assert!(!config_outside.is_night_time());
    }

    #[test]
    fn test_is_evolving_flag() {
        let config = night_config();
        let trigger = EvolutionTrigger::new(config);
        assert!(!trigger.is_evolving());
        trigger.set_evolving(true);
        assert!(trigger.is_evolving());
        trigger.set_evolving(false);
        assert!(!trigger.is_evolving());
    }
}
