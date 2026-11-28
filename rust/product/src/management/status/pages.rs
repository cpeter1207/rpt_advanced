//! Bounded HAL pagination of a single captured status view.
use super::{
    SNAPSHOT, STREAM,
    model::{Capture, Owner, RecordKey, Stamp},
    query::Selection,
};
use crate::management::{Api, Fault, Id};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Includes a margin for HAL links and framing below the listener's 4 MiB budget.
pub(super) const PAGE_BYTES: usize = 4 * 1024 * 1024 - 64 * 1024;
const CAPTURE_BYTES: usize = 24 * 1024 * 1024;

pub(super) struct Pages {
    status: Vec<Value>,
    stamp: Stamp,
    active_revision: Option<String>,
    cursor: u64,
    id: Id,
    selection: Selection,
    selector: String,
    bytes: usize,
}

pub(super) fn scoped(path: &str, query: &str) -> String {
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    }
}

impl Pages {
    pub(super) fn new(capture: Arc<Capture>, selection: Selection, id: Id) -> Result<Self, Fault> {
        Self::build(capture, selection, id, PAGE_BYTES)
    }

    fn build(
        capture: Arc<Capture>,
        selection: Selection,
        id: Id,
        page_limit: usize,
    ) -> Result<Self, Fault> {
        let mut status = Vec::new();
        let mut current = json!({});
        let mut total = 0usize;
        let mut page_size = 2usize;
        for (key, value) in capture
            .records
            .iter()
            .filter(|(record, _)| record.selected(&selection))
        {
            // Charge the full keyed path on each item, even where keys are shared on the page.
            let row_size = serialized_len(value.as_ref())
                .saturating_add(serialized_len(&key.path))
                .saturating_add(64);
            if row_size > page_limit {
                return Err(Fault::TooLarge);
            }
            if page_size.saturating_add(row_size) > page_limit {
                status.push(std::mem::replace(&mut current, json!({})));
                page_size = 2;
            }
            page_size = page_size.saturating_add(row_size);
            total = total.saturating_add(row_size);
            if total > CAPTURE_BYTES {
                return Err(Fault::TooLarge);
            }
            insert(&mut current, key, value.as_ref().clone());
        }
        status.push(current);
        let selector = selection.query();
        Ok(Self {
            status,
            stamp: capture.stamp.clone(),
            active_revision: capture.active_revision.clone(),
            cursor: capture.cursor,
            id,
            selection,
            selector,
            bytes: total,
        })
    }

    pub(super) fn selection(&self) -> &Selection {
        &self.selection
    }

    pub(super) fn len(&self) -> usize {
        self.status.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn document(
        &self,
        api: &Api,
        boot: Id,
        index: usize,
        stream: bool,
    ) -> Result<Value, Fault> {
        let status = self.status.get(index).ok_or(Fault::Missing)?;
        let complete = index + 1 == self.len();
        let snapshot = scoped(SNAPSHOT, &self.selector);
        let target = if index == 0 || stream {
            snapshot.clone()
        } else {
            self.continuation(index)
        };
        let mut links = api.links(&target, "status-snapshot");
        links["canonical"] = api.href(&snapshot);
        links["rpt:status-stream"] = api.href(&scoped(STREAM, &self.selector));
        links["rpt:status"] = api.href("/status/");
        if self.selection.site {
            links["rpt:ports"] = api.href("/status/ports/");
        }
        if !stream && !complete {
            links["next"] = api.href(&self.continuation(index + 1));
        }
        Ok(json!({
            "_links":links,
            "snapshot_id":self.id.to_string(),
            "boot_id":boot.to_string(),
            "active_revision":self.active_revision,
            "observed_at":self.stamp.utc,
            "monotonic_ms":self.stamp.monotonic_ms.to_string(),
            "state_cursor":self.cursor.to_string(),
            "selection":self.selection.document(),
            "page_index":index,
            "complete":complete,
            "status":status,
        }))
    }

    fn continuation(&self, index: usize) -> String {
        let prefix = if self.selector.is_empty() {
            String::new()
        } else {
            format!("{}&", self.selector)
        };
        scoped(
            SNAPSHOT,
            &format!("{prefix}cursor={}&page={index}", self.id),
        )
    }
}

fn serialized_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value)
        .expect("serializable public observation")
        .len()
}

fn insert(document: &mut Value, key: &RecordKey, data: Value) {
    let mut object = document.as_object_mut().expect("status map");
    match key.owner {
        Owner::Site => object = map_at(object, "site"),
        Owner::Port(port) => {
            object = map_at(map_at(object, "ports"), &port.to_string());
        }
    }
    let (name, parents) = key.path.split_last().expect("observation path");
    for parent in parents {
        object = map_at(object, parent);
    }
    object.insert(name.clone(), data);
}

fn map_at<'a>(object: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    object
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("disjoint public record keys")
}
