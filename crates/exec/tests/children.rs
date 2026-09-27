use std::time::Duration;

#[tokio::test]
async fn kill_all_stops_every_running_child() {
    let running = tokio::spawn(async move {
        mix_exec::Command::new("sleep")
            .arg("30")
            .output(&mix_exec::cancel::root())
            .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    mix_exec::group::kill_all();

    let output = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("a killed child ends at once")
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
}
