use std::time::Duration;

#[tokio::test]
async fn dispatch_receives_async_agent_notification() {
    let dispatch = brain_dispatch::TokioDispatch::new(64);
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(16);

    let dispatch_clone = dispatch.clone();
    let handle = tokio::spawn(async move {
        dispatch_clone.run_dispatch_loop(output_tx).await;
    });

    dispatch
        .inject(
            brain_dispatch::DispatchEvent::AsyncAgentCompleted(brain_dispatch::AgentResult {
                agent_id: "agent-001".into(),
                status: brain_dispatch::AgentStatus::Completed,
                output: "Found 3 files".into(),
                error: None,
                duration_ms: 1500,
            }),
            brain_dispatch::Priority::Background,
        )
        .await;

    let msg = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    match msg {
        brain_dispatch::MainLoopMessage::AgentNotification(result) => {
            assert_eq!(result.agent_id, "agent-001");
            assert_eq!(result.status, brain_dispatch::AgentStatus::Completed);
            assert_eq!(result.output, "Found 3 files");
            assert_eq!(result.duration_ms, 1500);
        }
        brain_dispatch::MainLoopMessage::BrainTaskNotification { .. } => {
            panic!("expected AgentNotification");
        }
    }

    dispatch.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn dispatch_receives_brain_task_notification() {
    let dispatch = brain_dispatch::TokioDispatch::new(64);
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(16);

    let dispatch_clone = dispatch.clone();
    let handle = tokio::spawn(async move {
        dispatch_clone.run_dispatch_loop(output_tx).await;
    });

    dispatch
        .inject(
            brain_dispatch::DispatchEvent::BrainTaskCompleted {
                brain_id: "evaluation".into(),
                result: brain_dispatch::AgentResult {
                    agent_id: "eval-001".into(),
                    status: brain_dispatch::AgentStatus::Completed,
                    output: "passed".into(),
                    error: None,
                    duration_ms: 500,
                },
            },
            brain_dispatch::Priority::Background,
        )
        .await;

    let msg = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    match msg {
        brain_dispatch::MainLoopMessage::BrainTaskNotification { brain_id, result } => {
            assert_eq!(brain_id, "evaluation");
            assert_eq!(result.status, brain_dispatch::AgentStatus::Completed);
        }
        brain_dispatch::MainLoopMessage::AgentNotification(_) => {
            panic!("expected BrainTaskNotification");
        }
    }

    dispatch.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[tokio::test]
async fn dispatch_prioritizes_urgent_events_over_background_batch() {
    let dispatch = brain_dispatch::TokioDispatch::new(64);
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(16);

    dispatch
        .inject(
            brain_dispatch::DispatchEvent::AsyncAgentCompleted(brain_dispatch::AgentResult {
                agent_id: "background-agent".into(),
                status: brain_dispatch::AgentStatus::Completed,
                output: "background".into(),
                error: None,
                duration_ms: 10,
            }),
            brain_dispatch::Priority::Background,
        )
        .await;
    dispatch
        .inject(
            brain_dispatch::DispatchEvent::AsyncAgentCompleted(brain_dispatch::AgentResult {
                agent_id: "urgent-agent".into(),
                status: brain_dispatch::AgentStatus::Completed,
                output: "urgent".into(),
                error: None,
                duration_ms: 5,
            }),
            brain_dispatch::Priority::Urgent,
        )
        .await;

    let dispatch_clone = dispatch.clone();
    let handle = tokio::spawn(async move {
        dispatch_clone.run_dispatch_loop(output_tx).await;
    });

    let msg = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    match msg {
        brain_dispatch::MainLoopMessage::AgentNotification(result) => {
            assert_eq!(result.agent_id, "urgent-agent");
        }
        brain_dispatch::MainLoopMessage::BrainTaskNotification { .. } => {
            panic!("expected AgentNotification");
        }
    }

    dispatch.shutdown().await;
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
}

#[test]
fn inject_sync_works_from_std_thread() {
    let dispatch = brain_dispatch::TokioDispatch::new(64);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(16);

    rt.block_on(async {
        let dispatch_clone = dispatch.clone();
        let handle = tokio::spawn(async move {
            dispatch_clone.run_dispatch_loop(output_tx).await;
        });

        let dispatch_clone2 = dispatch.clone();
        let thread = std::thread::spawn(move || {
            dispatch_clone2.inject_sync(
                "bg-agent-001".into(),
                "test-agent".into(),
                brain_dispatch::AgentStatus::Completed,
                "background task done".into(),
                None,
                2000,
            );
        });

        thread.join().unwrap();

        let msg = tokio::time::timeout(Duration::from_secs(2), output_rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        match msg {
            brain_dispatch::MainLoopMessage::AgentNotification(result) => {
                assert_eq!(result.agent_id, "bg-agent-001");
                assert_eq!(result.status, brain_dispatch::AgentStatus::Completed);
                assert!(result.output.contains("background task done"));
            }
            _ => panic!("expected AgentNotification"),
        }

        dispatch.shutdown().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    });
}

#[test]
fn sync_agent_channel_pattern() {
    let (tx, rx) = std::sync::mpsc::channel::<(String, Option<String>, Option<String>, u64)>();

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        tx.send(("completed".into(), Some("task result".into()), None, 50))
            .unwrap();
    });

    let (status, result, error, duration) = rx.recv().expect("channel closed");
    assert_eq!(status, "completed");
    assert_eq!(result.unwrap(), "task result");
    assert!(error.is_none());
    assert_eq!(duration, 50);
}
