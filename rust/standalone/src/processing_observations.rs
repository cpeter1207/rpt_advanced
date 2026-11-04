//! Retained immutable SDK observers; control never borrows an audio processor.

use crate::abi;
use std::{
    collections::BTreeMap,
    ffi::c_void,
    fmt::Write,
    mem::{offset_of, size_of},
    ptr::NonNull,
    sync::{Arc, Mutex, Weak},
};

type Read = unsafe extern "C" fn(*const c_void, *mut abi::UrpAstLinkObservation) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);

struct Lease {
    handle: NonNull<c_void>,
    read: Read,
    destroy: Destroy,
}
// SAFETY: this SDK owner exposes only atomic observations and independent destruction.
// Its Arc prevents destruction while any control reader is using the immutable lease.
unsafe impl Send for Lease {}
// SAFETY: every read uses the SDK's immutable observer API, never its audio graph handle.
unsafe impl Sync for Lease {}

impl Drop for Lease {
    fn drop(&mut self) {
        // SAFETY: creation transferred this unique observer allocation; all readers ended.
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}

pub(super) struct Observation {
    lease: Option<Lease>,
    unavailable: i32,
}

impl Observation {
    fn new(api: &abi::UrpAstDescriptor, graph: NonNull<c_void>) -> Self {
        let complete = api.struct_size as usize
            >= offset_of!(abi::UrpAstDescriptor, link_observer_destroy)
                + size_of::<Option<Destroy>>();
        if !complete {
            return Self {
                lease: None,
                unavailable: -7,
            };
        }
        let (Some(create), Some(read), Some(destroy)) = (
            api.link_observer_create,
            api.link_observer_read,
            api.link_observer_destroy,
        ) else {
            return Self {
                lease: None,
                unavailable: -7,
            };
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: preparation owns this graph before transferring it to audio callbacks.
        let status = unsafe { create(graph.as_ptr(), &mut handle) };
        let lease = NonNull::new(handle).map(|handle| Lease {
            handle,
            read,
            destroy,
        });
        if status == 0 && lease.is_some() {
            Self {
                lease,
                unavailable: 0,
            }
        } else {
            drop(lease);
            Self {
                lease: None,
                unavailable: if status == 0 { -1 } else { status },
            }
        }
    }

    fn append(&self, index: usize, text: &mut String) {
        let Some(lease) = &self.lease else {
            let _ = writeln!(
                text,
                "link_processor={index} observation_status=unavailable status={}",
                self.unavailable
            );
            return;
        };
        let mut snapshot = abi::UrpAstLinkObservation {
            struct_size: size_of::<abi::UrpAstLinkObservation>() as u32,
            abi_version: 1,
            processed_blocks: 0,
            bypassed_blocks: 0,
            failed_blocks: 0,
        };
        // SAFETY: the independent observer and writable record remain retained for this read.
        let status = unsafe { (lease.read)(lease.handle.as_ptr(), &mut snapshot) };
        if status != 0 {
            let _ = writeln!(
                text,
                "link_processor={index} observation_status=failed status={status}"
            );
        } else {
            let _ = writeln!(
                text,
                "link_processor={index} processed_blocks={} bypassed_blocks={} failed_blocks={}",
                snapshot.processed_blocks, snapshot.bypassed_blocks, snapshot.failed_blocks
            );
        }
    }
}

/// Weak node membership follows each processor's existing prepared/retired lifetime.
#[derive(Default)]
pub(crate) struct Observations(Mutex<BTreeMap<String, Vec<Weak<Observation>>>>);

impl Observations {
    pub(super) fn register(
        &self,
        node: &str,
        api: &abi::UrpAstDescriptor,
        graph: NonNull<c_void>,
    ) -> Arc<Observation> {
        let observation = Arc::new(Observation::new(api, graph));
        let mut nodes = self.0.lock().unwrap_or_else(|error| error.into_inner());
        nodes.retain(|_, observers| {
            observers.retain(|observer| observer.strong_count() != 0);
            !observers.is_empty()
        });
        nodes
            .entry(node.to_owned())
            .or_default()
            .push(Arc::downgrade(&observation));
        observation
    }

    pub(crate) fn text(&self, node: &str) -> String {
        let retained: Vec<_> = {
            let mut nodes = self.0.lock().unwrap_or_else(|error| error.into_inner());
            match nodes.get_mut(node) {
                Some(observers) => {
                    observers.retain(|observer| observer.strong_count() != 0);
                    observers.iter().filter_map(Weak::upgrade).collect()
                }
                None => Vec::new(),
            }
        };
        let mut text = format!("link_processors_retained={}\n", retained.len());
        for (index, observer) in retained.iter().enumerate() {
            observer.append(index + 1, &mut text);
        }
        text
    }
}
