use super::candidate_is_current;
use std::ptr::NonNull;

#[test]
fn candidate_check_rejects_null_and_replaced_generation_pointers() {
    let candidate = NonNull::<u8>::dangling().as_ptr();
    let replacement = candidate.wrapping_add(1);

    assert!(candidate_is_current(candidate, candidate));
    assert!(!candidate_is_current(std::ptr::null_mut(), candidate));
    assert!(!candidate_is_current(candidate, replacement));
}
