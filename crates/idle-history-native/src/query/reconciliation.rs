//! Capture replacement query windows without refreshing between pages.

use std::{collections::BTreeSet, io};

use editchain_core::activity::Operation;
use editchain_engine::queries::{ChainQueries, Lookup};

use super::{history, id, operation_details, search};
use idle_history::query::{HistoryPage, ItemSnapshot, Page, Reconcile, Reconciled, SearchPage};

pub(super) fn capture(queries: &ChainQueries, request: &Reconcile) -> io::Result<Reconciled> {
    if request.history_pages == 0 || (request.text.is_empty() != (request.search_pages == 0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid reconciliation window",
        ));
    }
    let history = scan_history(queries, request.history_pages, |op| {
        request.filter.matches(op)
    })?;
    let search = scan_search(queries, request)?;
    let items = request
        .items
        .iter()
        .map(|item| {
            let key = id(&item.item)?;
            Ok(ItemSnapshot {
                item: item.item.clone(),
                pages: scan_history(queries, item.pages, |op| {
                    Operation::view(op).map_or(op.id == key, |activity| activity.item.0 == key)
                })?,
            })
        })
        .collect::<io::Result<_>>()?;
    let mut details = Vec::new();
    let operations: BTreeSet<_> = request.known.iter().chain(&request.details).collect();
    for operation in operations {
        let identity = id(operation)?;
        if request.details.contains(operation)
            || !matches!(queries.operation(identity)?, Lookup::Found(_))
        {
            details.push(operation_details(queries, identity)?);
        }
    }
    Ok(Reconciled {
        history,
        search,
        items,
        details,
    })
}

fn scan_history(
    queries: &ChainQueries,
    count: u32,
    matches: impl Fn(&editchain_core::Op) -> bool,
) -> io::Result<Vec<HistoryPage>> {
    let mut pages = Vec::new();
    let mut request = Page::default();
    for _ in 0..count {
        let page = history(queries, &request, &matches)?;
        request.after.clone_from(&page.next_after);
        pages.push(page);
        if request.after.is_none() {
            break;
        }
    }
    Ok(pages)
}

fn scan_search(queries: &ChainQueries, input: &Reconcile) -> io::Result<Vec<SearchPage>> {
    let mut pages = Vec::new();
    let mut request = Page::default();
    for _ in 0..input.search_pages {
        let page = search(queries, &input.text, &input.filter, &request)?;
        request.after.clone_from(&page.next_after);
        pages.push(page);
        if request.after.is_none() {
            break;
        }
    }
    Ok(pages)
}
