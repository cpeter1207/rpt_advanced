//! A private account owns its authenticated registration, exact socket and call receipts.
use super::*;

pub(crate) struct Listener {
    server: IaxServer,
    route: Arc<Route>,
    id: String,
    port: String,
    registration: Option<(RegistrationReceipt, Instant, Instant)>,
}

/// A pending exact dial and the source socket from its registration receipt.
pub(super) struct PendingDial {
    native: records::PendingProviderDial,
    local: SocketAddr,
}

impl PendingDial {
    pub(super) fn cancel(&self) {
        self.native.cancel();
    }
}

fn dial_request<'a>(id: &'a str, generation: u64, destination: &'a str) -> DialRequest<'a> {
    DialRequest {
        account: id,
        generation,
        destination,
        caller_name: "",
        caller_number: "",
        context: "",
        timeout_ms: 10_000,
    }
}

impl Listener {
    pub(crate) fn bind(
        client: IaxClient,
        route: Arc<Route>,
        address: SocketAddr,
    ) -> Result<Self, IaxError> {
        if route.account.protocol() != crate::phone::Protocol::Iax2
            || route.remote.is_ipv4() != address.is_ipv4()
        {
            return Err(IaxError::InvalidParameters);
        }
        if !client
            .0
            .executor
            .supports_private(route.account.jitter_ms())
        {
            return Err(IaxError::IncompatibleAdapter);
        }
        let id = route.account.account_id().to_string();
        let port = route.account.port_id().to_string();
        let account = ProviderAccount {
            account: &id,
            generation: route.generation(),
            local_binding: &port,
            remote: route.remote,
            username: route.account.username(),
            secret: route.account.credential(),
            authentication_timeout_ms: 5000,
        };
        let server = IaxServer::bind_with_accounts(&client, address, &[], &[account])?;
        unsafe { client.0.executor.enable_server(server.handle.as_ptr()) }
            .map_err(|_| IaxError::Operation)?;
        Ok(Self {
            server,
            route,
            id,
            port,
            registration: None,
        })
    }
    pub(super) fn withdraw_local(&mut self) -> Result<(), IaxError> {
        self.server.configure_accounts(&[])
    }
    pub(crate) fn registered(&self) -> Option<RegistrationReceipt> {
        self.registration
            .as_ref()
            .filter(|(_, _, until)| Instant::now() < *until)
            .map(|(r, _, _)| *r)
    }
    pub(crate) fn refresh_due(&self) -> bool {
        self.registration
            .as_ref()
            .is_some_and(|(_, refresh, _)| Instant::now() >= *refresh)
    }
    pub(crate) fn register(&mut self, release: bool, timeout_ms: u32) -> Result<(), IaxError> {
        let previous = self.registration;
        // A release or failed renewal closes admission even if network retirement is uncertain.
        self.registration = None;
        let receipt = self.server.register_account(&RegistrationRequest {
            account: &self.id,
            generation: self.route.generation(),
            refresh_seconds: 300,
            release,
            timeout_ms,
        })?;
        if previous.is_some_and(|(old, _, _)| old.local != receipt.local) {
            return Err(IaxError::Operation);
        }
        if !release {
            let now = Instant::now();
            let valid = Duration::from_secs(u64::from(receipt.refresh_seconds));
            self.registration = Some((receipt, now + valid / 2, now + valid));
        }
        Ok(())
    }
    pub(super) fn supports_pending_dial(&self) -> bool {
        self.server.supports_pending_dial()
    }

    pub(super) fn begin_dial(&mut self, destination: &str) -> Result<PendingDial, IaxError> {
        let registration = self.registered().ok_or(IaxError::Rejected)?;
        let request = dial_request(&self.id, self.route.generation(), destination);
        let native = self.server.begin_provider_dial(&request)?;
        Ok(PendingDial {
            native,
            local: registration.local,
        })
    }

    pub(super) fn poll_dial(
        &self,
        dial: &mut PendingDial,
    ) -> Result<Option<ProviderCall>, IaxError> {
        dial.native
            .poll()?
            .map(|call| self.check_dial(call, dial.local))
            .transpose()
    }

    pub(crate) fn dial(&mut self, destination: &str) -> Result<ProviderCall, IaxError> {
        let registered = self.registered().ok_or(IaxError::Rejected)?;
        let request = dial_request(&self.id, self.route.generation(), destination);
        let call = self.server.dial_provider_account(&request)?;
        self.check_dial(call, registered.local)
    }

    fn check_dial(
        &self,
        call: ProviderCall,
        registered: SocketAddr,
    ) -> Result<ProviderCall, IaxError> {
        if self.registered().is_none()
            || !self.accepts(call.receipt, registered)
            || call.peer.sample_rate_hz() != 8000
        {
            return Err(IaxError::Rejected);
        }
        Ok(call)
    }
    fn accepts(&self, receipt: CallReceipt, local: SocketAddr) -> bool {
        receipt.account_generation == self.route.generation()
            && receipt.local == local
            && receipt.remote == self.route.remote
            && matches!(receipt.format, 4 | 8)
    }
    pub(crate) fn poll(&mut self) -> Result<Option<ProviderCall>, IaxError> {
        let now = u32::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| IaxError::Operation)?
                .as_secs(),
        )
        .map_err(|_| IaxError::Operation)?;
        // The synchronous callback may accept a newly owned peer while the
        // listener is mutably polling. Retain the same catalog and executor.
        let client = self.server._owner.clone();
        let mut state = Admission {
            local: self.registered().map(|r| r.local),
            id: &self.id,
            port: &self.port,
            route: &self.route,
            client: &client,
            call: None,
        };
        unsafe {
            self.server.poll_accounts(
                now,
                None,
                None,
                Some(authorize),
                Some(accept),
                ptr::from_mut(&mut state).cast(),
            )
        }?;
        Ok(state.call)
    }
}
struct Admission<'a> {
    local: Option<SocketAddr>,
    id: &'a str,
    port: &'a str,
    route: &'a Route,
    client: &'a IaxClient,
    call: Option<ProviderCall>,
}
impl Admission<'_> {
    unsafe fn receipt(
        &self,
        input: *const abi::rptadv_iax2_provider_incoming_v1,
    ) -> Option<CallReceipt> {
        if input.is_null()
            || unsafe { ptr::addr_of!((*input).struct_size).read_unaligned() }
                < size_of::<abi::rptadv_iax2_provider_incoming_v1>() as u32
        {
            return None;
        }
        let input = unsafe { &*input };
        let matches = |field: abi::rptadv_iax2_bytes_v1, expected: &str| {
            field.length == expected.len()
                && !field.data.is_null()
                && unsafe { std::slice::from_raw_parts(field.data, field.length) }
                    == expected.as_bytes()
        };
        if input.abi_version != 1
            || !matches(input.account, self.id)
            || !matches(input.local_binding, self.port)
            || !matches(input.username, self.route.account.username())
        {
            return None;
        }
        let receipt = records::call_receipt(input.receipt, self.route.generation()).ok()?;
        (Some(receipt.local) == self.local
            && receipt.remote == self.route.remote
            && matches!(receipt.format, 4 | 8))
        .then_some(receipt)
    }
}
unsafe extern "C" fn authorize(
    context: *mut c_void,
    input: *const abi::rptadv_iax2_provider_incoming_v1,
) -> i32 {
    let Some(state) = (unsafe { context.cast::<Admission<'_>>().as_ref() }) else {
        return -1;
    };
    if state.call.is_none() && unsafe { state.receipt(input) }.is_some() {
        0
    } else {
        -1
    }
}
unsafe extern "C" fn accept(
    context: *mut c_void,
    input: *const abi::rptadv_iax2_provider_incoming_v1,
    raw: *mut c_void,
) -> i32 {
    let Some(state) = (unsafe { context.cast::<Admission<'_>>().as_mut() }) else {
        return -1;
    };
    let Some(receipt) = (unsafe { state.receipt(input) }) else {
        return -1;
    };
    if state.call.is_some()
        || raw.is_null()
        || unsafe { state.client.0.api.sample_rate_hz.unwrap()(raw) } != 8000
    {
        return -1;
    }
    let Ok(peer) = (unsafe { state.client.clone().adopt_inbound(raw) }) else {
        return -1;
    };
    state.call = Some(ProviderCall { peer, receipt });
    0
}
