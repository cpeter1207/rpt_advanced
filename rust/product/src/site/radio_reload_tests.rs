//! Safe-release protocol reuses one existing owner and never pulses retained outputs.
use super::*;

struct Owner {
    state: abi::UrpAstSiteSnapshot,
    safe: u32,
    levels: Vec<u32>,
}
impl Owner {
    fn new() -> Self {
        Self {
            state: snapshot(),
            safe: 0,
            levels: Vec::new(),
        }
    }
    fn poll(
        &mut self,
        site: &mut RadioSite,
        now: u64,
    ) -> BTreeMap<String, rpt_advanced_core::site::SiteObservations> {
        site.poll(now, |_, _, operation| {
            match operation {
                Operation::Snapshot => return Ok(Some(self.state)),
                Operation::Bind(binding) => {
                    assert_eq!(binding.generation, self.state.generation);
                    let retained = self.state.claimed_mask & binding.output_mask;
                    let added = binding.output_mask & !self.state.claimed_mask;
                    let removed = self.state.claimed_mask & !binding.output_mask;
                    self.state.requested_high_mask = (self.state.requested_high_mask
                        & !(added | removed))
                        | (binding.initial_high_mask & added)
                        | (self.safe & removed);
                    assert_eq!(
                        self.state.requested_high_mask & retained,
                        self.state.submitted_high_mask & retained
                    );
                    self.state.claimed_mask = binding.output_mask;
                    self.state.generation += 13;
                    self.safe = binding.safe_high_mask;
                }
                Operation::Publish(request) => {
                    assert_eq!(request.generation, self.state.generation);
                    self.state.requested_high_mask =
                        (self.state.requested_high_mask & !request.output_mask) | request.high_mask;
                }
            }
            self.state.submitted_high_mask = self.state.requested_high_mask;
            self.state.submitted_generation = self.state.generation;
            self.levels.push(self.state.requested_high_mask);
            Ok(None)
        })
    }
    fn service(&mut self) {
        self.state.serviced_generation = self.state.submitted_generation;
        self.state.serviced_high_mask = self.state.submitted_high_mask;
    }
    fn settle(&mut self, site: &mut RadioSite, now: u64) {
        self.poll(site, now);
        self.service();
        self.poll(site, now + 1);
    }
}

#[test]
fn site_radio_reload_preserves_retained_level_and_waits_for_safe_before_release() {
    let mut definitions = settings();
    let mut second = definitions["2000"].outputs[0].clone();
    second.name = "retained".into();
    second.active_high = true;
    second.endpoint.as_mut().unwrap().channel = "8".into();
    definitions.get_mut("2000").unwrap().outputs.push(second);
    let mut site = RadioSite::prepare(&definitions).unwrap();
    let mut owner = Owner::new();
    owner.settle(&mut site, 0);
    assert_eq!(owner.state.serviced_high_mask, 0x80);
    definitions.get_mut("2000").unwrap().outputs.remove(0);
    assert!(site.begin(
        RadioSite::prepare(&definitions).unwrap(),
        &Default::default()
    ));
    owner.poll(&mut site, 10);
    assert_eq!(owner.state.submitted_high_mask, 0xc0);
    assert_eq!(owner.state.claimed_mask, 0xc0);
    assert_eq!(site.barrier(), None);
    assert!(!site.stage());
    owner.service();
    owner.poll(&mut site, 11);
    assert_eq!(site.barrier(), Some(Ok(())));
    assert!(site.stage());
    owner.poll(&mut site, 12);
    assert_eq!(owner.state.claimed_mask, 0x80);
    assert!(!site.commit());
    owner.service();
    owner.poll(&mut site, 13);
    assert!(site.commit());
    owner.settle(&mut site, 14);
    assert!(owner.levels.iter().all(|high| high & 0x80 != 0));
    assert_eq!(
        site.output_status("2000", "retained").unwrap().requested,
        1.0
    );
    assert!(site.output_status("2000", "relay").is_none());
    assert!(site.begin(RadioSite::default(), &Default::default()));
    owner.settle(&mut site, 20);
    assert_eq!(owner.state.serviced_high_mask & 0x80, 0);
    assert!(site.stage());
    owner.settle(&mut site, 22);
    assert!(site.commit());
    assert_eq!(owner.state.claimed_mask, 0);
}

#[test]
fn site_radio_reload_abort_restores_demand_and_stalled_owner_does_not_release() {
    let mut site = RadioSite::prepare(&settings()).unwrap();
    let mut owner = Owner::new();
    owner.settle(&mut site, 0);
    assert!(site.begin(RadioSite::default(), &Default::default()));
    owner.poll(&mut site, 10);
    owner.poll(&mut site, 5010);
    assert_eq!(site.barrier(), Some(Err(ReadingQuality::Stale)));
    assert!(!site.stage());
    assert_eq!(owner.state.claimed_mask, 0x40);
    site.abort();
    owner.service();
    owner.settle(&mut site, 5011);
    assert_eq!(owner.state.serviced_high_mask & 0x40, 0);
    assert_eq!(site.output_status("2000", "relay").unwrap().requested, 1.0);
    // A new radio profile must relinquish even unchanged pins before old owner teardown.
    assert!(site.begin(
        RadioSite::prepare(&settings()).unwrap(),
        &["1000".into()].into_iter().collect()
    ));
    owner.settle(&mut site, 5020);
    assert!(site.stage());
    owner.settle(&mut site, 5022);
    assert_eq!(owner.state.claimed_mask, 0);
    assert!(site.commit());
    owner.settle(&mut site, 5024);
    assert_eq!(owner.state.claimed_mask, 0x40);
}

#[test]
fn site_radio_recovery_preserves_ack_time_and_retries_only_after_owner_progress() {
    let mut site = RadioSite::prepare(&settings()).unwrap();
    let mut owner = Owner::new();
    owner.settle(&mut site, 0);
    let acknowledged = site.output_status("2000", "relay").unwrap().submitted;
    site.poll(10, |_, _, _| Err(-7));
    assert_eq!(
        site.output_status("2000", "relay").unwrap().quality,
        ReadingQuality::Unavailable
    );
    owner.poll(&mut site, 11);
    assert_eq!(
        site.output_status("2000", "relay").unwrap().quality,
        ReadingQuality::Valid
    );
    assert_eq!(
        site.output_status("2000", "relay").unwrap().submitted,
        acknowledged
    );
    site.output("2000", "relay", 0.0).unwrap();
    owner.poll(&mut site, 12);
    let attempts = owner.levels.len();
    owner.poll(&mut site, 5012);
    assert_eq!(owner.levels.len(), attempts);
    assert_eq!(
        site.output_status("2000", "relay").unwrap().quality,
        ReadingQuality::Stale
    );
    owner.state.generation = 0;
    owner.state.claimed_mask = 0;
    owner.poll(&mut site, 5013);
    owner.state.generation = 100;
    owner.settle(&mut site, 5014);
    assert_eq!(
        site.output_status("2000", "relay")
            .unwrap()
            .submitted
            .unwrap()
            .0,
        0.0
    );
}

#[test]
fn site_radio_unacquired_fault_does_not_require_release_or_open_on_shutdown() {
    let mut site = RadioSite::prepare(&settings()).unwrap();
    site.poll(0, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Err(-7)
    });
    assert!(site.begin(RadioSite::default(), &Default::default()));
    assert_eq!(site.barrier(), Some(Ok(())));
    assert!(site.stage());
    site.poll(1, |_, _, op| {
        assert!(matches!(op, Operation::Snapshot));
        Err(-7)
    });
    assert!(site.commit());
}
