use gpui::BenchAppContext;

#[gpui::bench(inputs = [64_usize, 2048, 8192], input_name = "updates", group = "Remote snapshot backlog", sample_size = 10)]
fn remote_snapshot(update_count: &usize, cx: &mut BenchAppContext) {
    let report = worktree::benchmark_snapshot_updates(*update_count, 128)
        .expect("snapshot benchmark must preserve entries and scan completion");
    println!(
        "retained snapshots: {}, delivered updates: {}, final entries: {}",
        report.retained_snapshots, report.delivered_updates, report.final_entries
    );
    cx.bench_iter(|_| {
        std::hint::black_box(
            worktree::benchmark_snapshot_updates(*update_count, 128)
                .expect("snapshot benchmark must preserve entries and scan completion"),
        );
    });
}

gpui::bench_group!(benches, remote_snapshot);
gpui::bench_main!(benches);
