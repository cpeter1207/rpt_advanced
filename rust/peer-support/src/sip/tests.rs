//! Memory-layout admission contract, excluded as test code by the normal gate.
#[test]
fn fixed_header_index_stays_within_its_admission_budget() {
    assert!(std::mem::size_of::<super::Field>() <= 16);
    eprintln!(
        "header_index_entry={} count={}",
        std::mem::size_of::<super::Field>(),
        super::MAX_FIELDS
    );
}
