use super::*;

fn stream(key: &str, time: u64, parents: &[&str], source: &str) -> LiveBlockMeta {
    LiveBlockMeta {
        human_stream: Some(source.into()),
        ..node(key, time, parents)
    }
}

#[test]
fn nested_sources_keep_their_ancestors_columns_across_batch_boundaries() {
    let nodes = [
        stream("main", 1, &[], "main"),
        stream("child", 2, &["main"], "child"),
        stream("grandchild", 3, &["child"], "grandchild"),
        stream("main2", 4, &["main"], "main"),
        stream("grandchild2", 5, &["grandchild"], "grandchild"),
        stream("child_join", 6, &["child", "grandchild2"], "child"),
        stream("main_join", 7, &["main2", "child_join"], "main"),
    ];
    for size in [1, 2, nodes.len()] {
        let mut graph = LiveGraph::default();
        for batch in nodes.chunks(size) {
            graph.edit_stream(&[], batch);
            for key in graph.ordered_keys() {
                let expected = match key.as_str() {
                    "main" | "main2" | "main_join" => Some(0),
                    "child" | "child_join" => Some(1),
                    "grandchild" | "grandchild2" => Some(2),
                    _ => None,
                };
                assert_eq!(
                    Some(row(&graph, key, 0).lane),
                    expected,
                    "{key}, batch {size}"
                );
            }
        }
    }
}

#[test]
fn independent_streams_never_form_a_visual_join() {
    let nodes = [
        stream("a", 1, &[], "a"),
        stream("b", 2, &[], "b"),
        stream("a2", 3, &["a"], "a"),
        stream("b2", 4, &["b"], "b"),
    ];
    for size in [1, nodes.len()] {
        let mut graph = LiveGraph::default();
        for batch in nodes.chunks(size) {
            graph.edit_stream(&[], batch);
        }
        for (key, lane) in [("a", 0), ("a2", 0), ("b", 1), ("b2", 1)] {
            let geometry = row(&graph, key, 0);
            assert_eq!(geometry.lane, lane, "{key} stays on its stream");
            assert!(
                geometry.transitions.is_empty(),
                "{key} has no cross-stream attachment"
            );
        }
    }
}

#[test]
fn a_reopened_stream_cannot_route_through_the_new_column_owner() {
    let mut ended = stream("a_end", 2, &["a"], "a");
    ended.source_closed = true;
    let mut graph = LiveGraph::default();
    graph.edit_stream(&[], &[stream("a", 1, &[], "a"), ended.clone()]);
    graph.edit_stream(&[], &[stream("b", 3, &[], "b")]);
    assert_eq!(
        row(&graph, "b", 0).lane,
        0,
        "an explicit end permits column reuse"
    );
    // Repairing an old end must not steal its new owner's reservation.
    graph.edit_stream(&[], &[ended]);
    graph.edit_stream(&[], &[stream("a_resume", 4, &["a_end"], "a")]);
    assert_eq!(
        row(&graph, "a_resume", 0).lane,
        1,
        "the intervening owner is protected"
    );
    assert!(
        row(&graph, "b", 0).transitions.is_empty(),
        "the passing edge cannot attach to b"
    );
    assert_eq!(
        row(&graph, "a_end", 0).transitions,
        vec![(1, 0)],
        "the bend reaches the actual parent"
    );
    graph.edit_stream(&[], &[stream("b2", 5, &["b"], "b")]);
    assert_eq!(row(&graph, "b2", 0).lane, 0, "b keeps its active column");
}

#[test]
fn retracting_an_end_does_not_infer_that_the_source_is_closed() {
    let mut ended = stream("end", 2, &["a"], "a");
    ended.source_closed = true;
    let mut graph = LiveGraph::default();
    graph.edit_stream(&[], &[stream("a", 1, &[], "a"), ended]);
    graph.edit_stream(&["end".into()], &[]);
    graph.edit_stream(&[], &[stream("b", 3, &[], "b")]);
    assert_eq!(
        row(&graph, "b", 0).lane,
        1,
        "a remains reserved without a recorded end"
    );
}

#[test]
fn a_late_unrelated_record_cannot_take_a_completed_branch_turn_boundary() {
    let mut child = stream("child_end", 3, &["parent"], "child");
    child.source_closed = true;
    let mut graph = LiveGraph::default();
    graph.edit_stream(&[], &[stream("parent", 1, &[], "parent"), child]);
    graph.edit_stream(&[], &[stream("late", 2, &[], "other")]);
    assert_eq!(
        row(&graph, "child_end", 0).lane,
        1,
        "the completed branch remains stable"
    );
    assert_eq!(
        row(&graph, "late", 0).lane,
        2,
        "a passing turn occupies its top boundary even when its source has ended"
    );
    assert!(
        row(&graph, "late", 0)
            .transitions
            .iter()
            .all(|(from, to)| *from != 2 && *to != 2),
        "the late record has no invented attachment"
    );
}
