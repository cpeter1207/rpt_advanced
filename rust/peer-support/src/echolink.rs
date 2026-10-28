//! Shared EchoLink descriptor validation, with no protocol or media implementation.

pub mod abi;

/// Provider identity, layout or advertised operation group is not usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiError;

/// Copied immutable operations; the host retains the provider DSO through final detach.
#[derive(Clone, Copy)]
pub struct Api(abi::Descriptor);

impl Api {
    /// Validate every advertised operation before invoking any provider function.
    ///
    /// # Safety
    /// A nonnull pointer exposes its truthful readable size prefix and the complete
    /// initialized record when its size suffices. Code remains loaded while this
    /// copied table or any of its provider objects is used. No callback may unwind.
    pub unsafe fn open(pointer: *const abi::Descriptor) -> Result<Self, ApiError> {
        if pointer.is_null()
            || unsafe { std::ptr::addr_of!((*pointer).struct_size).read() }
                < std::mem::size_of::<abi::Descriptor>() as u32
        {
            return Err(ApiError);
        }
        // SAFETY: the truthful size prefix covers the complete immutable record.
        let table = unsafe { pointer.read() };
        if table.abi_version != 1
            || table.capability != *b"rptadv.echolink.v1\0\0\0\0\0\0"
            || table.capabilities & 1 == 0
            || table.capabilities & !3 != 0
            || ![
                table.context_create.is_some(),
                table.context_destroy.is_some(),
                table.account_prepare.is_some(),
                table.account_stop.is_some(),
                table.account_destroy.is_some(),
            ]
            .into_iter()
            .all(|present| present)
        {
            return Err(ApiError);
        }
        if table.capabilities & 2 != 0
            && ![
                table.account_activate.is_some(),
                table.account_reconfigure.is_some(),
                table.dial.is_some(),
                table.validate_pending.is_some(),
                table.peer_info.is_some(),
                table.peer_attach.is_some(),
                table.peer_stop.is_some(),
                table.peer_detach.is_some(),
                table.peer_wake.is_some(),
                table.peer_set_deadline.is_some(),
            ]
            .into_iter()
            .all(|present| present)
        {
            return Err(ApiError);
        }
        Ok(Self(table))
    }

    /// True only when the provider advertises a complete network operation group.
    pub fn network_available(&self) -> bool {
        self.0.capabilities & 2 != 0
    }

    /// Borrow validated operations; each unsafe call still obeys its ownership contract.
    pub fn table(&self) -> &abi::Descriptor {
        &self.0
    }
}
