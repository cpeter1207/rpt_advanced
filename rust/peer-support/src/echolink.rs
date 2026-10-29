//! Shared EchoLink descriptor validation, with no protocol or media implementation.

pub mod abi;
pub mod host;

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
                < std::mem::offset_of!(abi::Descriptor, peer_destroy) as u32
        {
            return Err(ApiError);
        }
        // The lifecycle prefix predates the compatible destruction tail. Every
        // field is an integer, byte or nullable function pointer, so zero is a
        // valid absent table. Copy only a complete, truthfully readable group.
        let mut table: abi::Descriptor = unsafe { std::mem::zeroed() };
        let length = [
            std::mem::offset_of!(abi::Descriptor, peer_destroy),
            std::mem::offset_of!(abi::Descriptor, peer_read),
            std::mem::offset_of!(abi::Descriptor, peer_send_text),
            std::mem::offset_of!(abi::Descriptor, account_apply_prepared),
            std::mem::size_of::<abi::Descriptor>(),
        ]
        .into_iter()
        .filter(|length| *length <= unsafe { (*pointer).struct_size } as usize)
        .max()
        .unwrap(); // The minimum lifecycle prefix was checked above.
        unsafe {
            std::ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                std::ptr::from_mut(&mut table).cast(),
                length,
            )
        };
        if table.abi_version != 1
            || table.capability != *b"rptadv.echolink.v1\0\0\0\0\0\0"
            || table.capabilities & 1 == 0
            || table.capabilities & !15 != 0
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
                table.account_apply_prepared.is_some(),
                table.dial.is_some(),
                table.validate_pending.is_some(),
                table.peer_info.is_some(),
                table.peer_attach.is_some(),
                table.peer_stop.is_some(),
                table.peer_detach.is_some(),
                table.peer_wake.is_some(),
                table.peer_set_deadline.is_some(),
                table.peer_destroy.is_some(),
            ]
            .into_iter()
            .all(|present| present)
        {
            return Err(ApiError);
        }
        if table.capabilities & 4 != 0
            && (table.capabilities & 2 == 0
                || ![
                    table.peer_read.is_some(),
                    table.peer_prepare_audio.is_some(),
                    table.peer_reset_encoder.is_some(),
                    table.peer_send_prepared.is_some(),
                    table.peer_elapsed_ms.is_some(),
                ]
                .into_iter()
                .all(|present| present))
        {
            return Err(ApiError);
        }
        if table.capabilities & 8 != 0
            && (table.capabilities & 4 == 0 || table.peer_send_text.is_none())
        {
            return Err(ApiError);
        }
        Ok(Self(table))
    }

    /// True only when the provider advertises a complete network operation group.
    pub fn network_available(&self) -> bool {
        self.0.capabilities & 2 != 0
    }

    /// True only for the complete source-rate receive/prepared-egress group.
    pub fn prepared_media_available(&self) -> bool {
        self.0.capabilities & 4 != 0
    }

    /// True only for human text on a complete network/media provider.
    pub fn text_available(&self) -> bool {
        self.0.capabilities & 8 != 0
    }

    /// Borrow validated operations; each unsafe call still obeys its ownership contract.
    pub fn table(&self) -> &abi::Descriptor {
        &self.0
    }
}
