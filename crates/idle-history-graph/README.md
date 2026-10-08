# idle-history-graph

Portable activity ordering, causal lanes and retained routes for native and WASM
clients. `live::LiveGraph` consumes generic `GraphNode` inputs and returns row
boundaries, lane numbers and route connections through `RowGeometry`.

The runtime depends on `editchain-index` and Serde. It owns no application state,
UI framework, browser API, pixel coordinates or animation. Native timeline
queries consume it directly; web-ui adds viewport measurements, SVG paths and
motion. The `history-geometry` package re-exports the existing `live` API for
browser callers.

Graph and route regressions live beside the implementation, including recorded
layout fixtures, source lifetimes, task boundaries and batched window reads.
