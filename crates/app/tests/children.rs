use std::time::Duration;

#[tokio::test]
async fn kill_all_stops_every_running_child() {
    let mut sleep = mix_app::command("sleep");
    sleep.arg("30");
    let running = tokio::spawn(async move {
        mix_app::output(sleep, "sleep 30", None, &mix_core::cancel::root()).await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    mix_app::children::kill_all();

    let output = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("a killed child ends at once")
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
}
