use std::{ops::Range, sync::Arc, time::Duration};

use gpui::{AppContext as _, BenchAppContext};
use language::{Buffer, BufferSnapshot, OffsetRangeExt as _};
use project::{project_search::benchmark_find_buffer_matches, search::SearchQuery};
use settings::Settings as _;

fn match_counts() -> Vec<usize> {
    vec![100, 10_000, 100_000, 1_000_000]
}

struct ValidatedMatches {
    snapshot: BufferSnapshot,
    ranges: Vec<Range<language::Anchor>>,
    match_count: usize,
}

impl Drop for ValidatedMatches {
    fn drop(&mut self) {
        let prefix_count = self.match_count.min(10_001);
        assert!(self.ranges.len() >= prefix_count);
        assert!(self.ranges.len() <= self.match_count);
        for (index, range) in self.ranges.iter().take(prefix_count).enumerate() {
            assert_eq!(range.to_offset(&self.snapshot), index * 7..index * 7 + 6);
        }
    }
}

#[gpui::bench(inputs = match_counts(), input_name = "matches", group = "Search budget")]
fn dense_literal(match_count: &usize, cx: &mut BenchAppContext) {
    cx.update(|cx| {
        settings::init(cx);
        language::language_settings::AllLanguageSettings::register(cx);
    });
    let buffer = cx.update(|cx| cx.new(|cx| Buffer::local("needle\n".repeat(*match_count), cx)));
    let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
    let query = Arc::new(
        SearchQuery::text(
            "needle",
            false,
            true,
            false,
            Default::default(),
            Default::default(),
            false,
            None,
        )
        .expect("valid literal query"),
    );
    let match_count = *match_count;
    // The output's Drop validates the prefix after the harness stops timing.
    cx.bench_task(|cx| {
        let snapshot = snapshot.clone();
        let query = query.clone();
        cx.background_executor().spawn(async move {
            let ranges = benchmark_find_buffer_matches(&query, &snapshot).await;
            ValidatedMatches {
                snapshot,
                ranges,
                match_count,
            }
        })
    });
}

#[gpui::bench(inputs = match_counts(), input_name = "matches", group = "Unlimited search control")]
fn unlimited_literal(match_count: &usize, cx: &mut BenchAppContext) {
    cx.update(|cx| {
        settings::init(cx);
        language::language_settings::AllLanguageSettings::register(cx);
    });
    let buffer = cx.update(|cx| cx.new(|cx| Buffer::local("needle\n".repeat(*match_count), cx)));
    let snapshot = cx.update(|cx| buffer.read(cx).snapshot());
    let query = Arc::new(
        SearchQuery::text(
            "needle",
            false,
            true,
            false,
            Default::default(),
            Default::default(),
            false,
            None,
        )
        .expect("valid literal query"),
    );
    let match_count = *match_count;
    cx.bench_task(|cx| {
        let snapshot = snapshot.clone();
        let query = query.clone();
        cx.background_executor().spawn(async move {
            let ranges = query.search(&snapshot, None).await;
            assert_eq!(ranges.len(), match_count);
            assert_eq!(ranges.first(), Some(&(0..6)));
            assert_eq!(
                ranges.last(),
                Some(&((match_count - 1) * 7..(match_count - 1) * 7 + 6))
            );
            ranges
        })
    });
}

gpui::bench_group! {
    name = benches;
    config = criterion::Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(1))
        .without_plots();
    targets = dense_literal, unlimited_literal
}
gpui::bench_main!(benches);
