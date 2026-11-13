//! Focused state, generation and fixed-retention regression tests.
use super::*;

fn observations() -> Observations {
    Observations {
        status: Status {
            struct_size: size_of::<Status>() as u32,
            state: 1,
            generation: 7,
            sequence: 0,
            sip_status: 0,
            accepted_expires: 0,
            registrations: 0,
            events_dropped: 0,
        },
        events: VecDeque::with_capacity(8),
        authenticate: false,
        challenges: 0,
        realm: [0; 256],
        realm_length: 0,
    }
}

#[test]
fn callbacks_retain_exact_generation_and_only_eight_numeric_events() {
    let mut state = observations();
    for _ in 0..1000 {
        state.observe(1, 200, 4);
    }
    assert_eq!(state.status.registrations, 1000);
    assert_eq!(state.status.generation, 7);
    assert_eq!(state.status.events_dropped, 992);
    assert_eq!(state.events.len(), 8);
    assert_eq!(state.events.front().unwrap().sequence, 993);
    assert_eq!(state.events.back().unwrap().sequence, 1000);
}

#[test]
fn digest_failure_is_terminal_and_shutdown_failure_retains_lease_state() {
    let mut state = observations();
    state.observe(1, 401, 0);
    assert!(state.authenticate);
    assert_eq!(state.status.state, 1);
    state.observe(1, 401, 0);
    assert_eq!(state.status.state, 5);
    state.observe(1, 200, 60);
    assert_eq!(state.status.state, 5);
    state.observe(3, 500, 0);
    assert_eq!(state.status.state, 8);
}

#[test]
fn prepared_stopping_and_unregistered_accounts_ignore_late_register_events() {
    for initial in [0, 4, 5, 6, 7, 8] {
        let mut state = observations();
        state.status.state = initial;
        state.observe(1, 200, 60);
        assert_eq!(state.status.state, initial);
        assert!(state.events.is_empty());
    }
}

#[test]
fn invalid_accepted_expiry_fails_closed_and_successful_removal_stops_refresh() {
    for expiry in [0, 86401, u32::MAX] {
        let mut state = observations();
        state.observe(1, 200, expiry);
        assert_eq!(state.status.state, 5);
    }
    let mut state = observations();
    state.observe(1, 100, 0);
    assert!(state.events.is_empty());
    state.status.state = 3;
    state.observe(2, 200, 0);
    assert_eq!(state.status.state, 4);
    state.observe(3, 200, 0);
    assert_eq!(state.status.state, 7);
}

#[test]
fn uri_injection_and_unexpected_native_routes_are_refused() {
    for input in [
        "",
        "sips:example.test",
        "sip:a@example.test",
        "sip:example.test;transport=tcp",
        "sip:example.test?Subject=secret",
        "sip:example.test\r\nVia: forged",
    ] {
        assert!(sip_uri(input, false).is_err());
    }
    assert!(sip_uri("sip:example.test:5060", false).is_ok());
    assert!(sip_uri("sip:account@example.test", true).is_ok());
    assert!(sip_uri("sip:@example.test", true).is_err());
    assert!(sip_uri("sip:example.test", true).is_err());
    assert!(sip_uri("sip:user:password@example.test", true).is_err());
}

#[test]
fn late_refresh_does_not_undo_pending_unregister() {
    for code in [200, 401, 403, 407] {
        let mut state = observations();
        state.status.state = 3;
        state.observe(1, code, 4);
        assert_eq!(
            state.status.state, 3,
            "late refresh response replaced removal"
        );
        assert!(state.events.is_empty());
        assert_eq!(state.authenticate, matches!(code, 401 | 407));
        state.observe(2, 200, 0);
        assert_eq!(state.status.state, 4);
    }
}

#[test]
fn outbound_failure_closes_registered_account_but_not_pending_removal() {
    let mut state = observations();
    state.status.state = 2;
    state.observe(4, 408, 0);
    assert_eq!(state.status.state, 5);
    assert_eq!(state.status.sip_status, 408);
    let mut removing = observations();
    removing.status.state = 3;
    removing.observe(4, 408, 0);
    assert_eq!(removing.status.state, 3);
}

#[test]
fn callbacks_survive_owner_moves_and_repeated_host_accesses() {
    let account = Box::new(Account {
        native: ptr::null_mut(),
        observed: Box::new(UnsafeCell::new(observations())),
        registrar: CString::new("sip:fixture.invalid").unwrap(),
        aor: CString::new("sip:user@fixture.invalid").unwrap(),
        hop: CString::new("sip:127.0.0.1:9").unwrap(),
        activation_pending: false,
        expires: CString::new("60").unwrap(),
        auth: Secret(vec![0]),
    });
    let callback = account.observed.get().cast();
    let mut owners = vec![account];
    let account = owners.pop().unwrap();
    let mut account = account;
    for _ in 0..16 {
        account.observed_mut().publish(1, 0, 0);
        // SAFETY: simulate the retained native pointer during serialized service.
        unsafe { observe(callback, 1, 200, 4, ptr::null(), 0) };
        assert_eq!(account.observed().status.state, 2);
        assert!(account.observed_mut().events.pop_front().is_some());
    }
}

#[test]
fn native_authentication_acknowledgement_preserves_pending_operation() {
    for pending in [1, 3] {
        let mut state = observations();
        state.status.state = pending;
        state.observe(5, 202, 0);
        assert_eq!(state.status.state, pending);
        assert!(state.events.is_empty());
        state.observe(5, 900, 0);
        assert_eq!(state.status.state, 5);
        assert_eq!(state.status.sip_status, 900);
    }
}

#[test]
fn callback_copies_only_bounded_realm_and_rejects_missing_challenge() {
    for realm in [b"\"fixture\"".as_slice(), b"", &[b'x'; 257]] {
        let storage = UnsafeCell::new(observations());
        // SAFETY: owned storage and synthetic borrowed bytes live through the call.
        unsafe {
            observe(
                storage.get().cast(),
                1,
                401,
                0,
                realm.as_ptr().cast(),
                realm.len(),
            )
        };
        let state = storage.into_inner();
        if realm.len() == 9 {
            assert!(state.authenticate);
            assert_eq!(&state.realm[..state.realm_length], realm);
            assert_eq!(state.status.state, 1);
        } else {
            assert!(!state.authenticate);
            assert_eq!(state.status.state, 5);
        }
    }
}

#[test]
fn exhausted_refresh_authentication_fails_pending_removal() {
    for challenge in [401, 407] {
        let mut state = observations();
        state.status.state = 3;
        state.observe(1, challenge, 0);
        assert!(state.authenticate);
        assert_eq!(state.status.state, 3);
        state.observe(1, challenge, 0);
        assert!(!state.authenticate);
        assert_eq!(
            state.status.state, 5,
            "native credential wait must be cancelled"
        );
        assert_eq!(state.status.sip_status, challenge);
        state.observe(2, 200, 0);
        assert_eq!(state.status.state, 5);
    }
}

#[test]
fn first_refresh_challenge_observes_authentication_once_without_resetting_it() {
    let mut state = observations();
    state.observe(1, 200, 4);
    let previous = state.status.sequence;
    state.observe(1, 401, 0);
    assert_eq!(state.status.state, 1);
    assert_eq!(state.status.sequence, previous + 1);
    assert_eq!(state.status.accepted_expires, 4);
    assert_eq!(state.status.registrations, 1);
    assert!(state.authenticate);
    state.observe(1, 407, 0);
    assert_eq!(state.status.state, 5);
    assert_eq!(state.status.sequence, previous + 2);
    let mut initial = observations();
    initial.observe(1, 401, 0);
    assert_eq!(
        initial.status.sequence, 0,
        "initial auth deadline must not restart"
    );
}

#[test]
fn aor_accepts_valid_percent_encoded_utf8_user_only() {
    assert!(sip_uri("sip:%C3%A9@example.test", true).is_ok());
    for text in [
        "sip:%@example.test",
        "sip:%3@example.test",
        "sip:%GH@example.test",
    ] {
        assert!(sip_uri(text, true).is_err());
    }
}
