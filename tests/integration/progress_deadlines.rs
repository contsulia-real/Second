use crate::support::progress::ProgressDeadline;
use std::time::Duration;
use tokio::time::Instant;

#[test]
fn progressing_work_outlives_one_idle_budget_but_stalls_and_endless_churn_fail() {
    let start = Instant::now();
    let mut deadline =
        ProgressDeadline::new(start, Duration::from_secs(5), Duration::from_secs(20));
    for second in [0, 4, 8, 12] {
        deadline
            .observe(start + Duration::from_secs(second), second)
            .unwrap();
    }
    assert!(
        deadline
            .observe(start + Duration::from_secs(17), 12)
            .unwrap_err()
            .contains("no progress")
    );
    // Advancing generations or changing connections cannot renew the hard limit.
    assert!(
        deadline
            .observe(start + Duration::from_secs(20), 20)
            .unwrap_err()
            .contains("overall deadline")
    );
    // No progress from the very first observation uses the same idle limit.
    let mut startup = ProgressDeadline::new(start, Duration::from_secs(5), Duration::from_secs(20));
    startup.observe(start, 0).unwrap();
    startup.observe(start + Duration::from_secs(4), 0).unwrap();
    assert!(
        startup
            .observe(start + Duration::from_secs(5), 0)
            .unwrap_err()
            .contains("no progress")
    );
}
