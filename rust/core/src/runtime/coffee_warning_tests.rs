use super::*;

const START_WARNING: &str = "warn_start_handiham_radio_club_net";
const END_WARNING: &str = "warn_end_handiham_radio_club_net";

fn warning_configuration() -> Arc<crate::config::resources::generation::RuntimeConfiguration> {
    let mut rows = replacement_resources().to_vec();
    let (_, _, _, handi) = rows
        .iter_mut()
        .find(|row| row.2 == "handiham_radio_club_net")
        .unwrap();
    handi["start"]["warnings"] = json!([{
        "at":"every weekday at 10:59 in America/Chicago", "macro":START_WARNING
    }]);
    handi["end"]["warnings"] = json!([{
        "at":"every weekday at 11:00 in America/Chicago", "macro":END_WARNING
    }]);
    for (key, verb) in [
        (START_WARNING, "Connecting to"),
        (END_WARNING, "Disconnecting from"),
    ] {
        let message = format!("{verb} Handiham Radio Club Net in {{ $time-remaining }}");
        rows.push((PORT, ResourceKind::Message, key, json!({"locales":{"en-US":{
            "text":message, "tts":message, "morse":format!("{} HANDIHAM RADIO CLUB NET IN {{ $time-remaining }}", verb.to_uppercase())
        }}})));
        rows.push((
            PORT,
            ResourceKind::Macro,
            key,
            json!({"actions":[{
                "action":"message","destination":"local","message":format!("{{ {key} }}")
            }]}),
        ));
    }
    rows.push((
        PORT,
        ResourceKind::TelemetrySettings,
        "",
        json!({
            "speech":{"voice":"/usr/lib/piper-tts/voices/en_US-lessac-high.onnx","level_db":-6},
            "morse":{"level_db":-24},"timing":{"courtesy_delay_ms":500}
        }),
    ));
    station_fixture::configuration(&[(PORT, "station")], &rows)
}

struct WarningMedia {
    messages: Arc<Mutex<Vec<String>>>,
    reject_end_warning: bool,
}

impl NativeFilePreparer for WarningMedia {
    fn file(&self, request: &FileRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Media.file(request)
    }
}
impl NativeSpeechPreparer for WarningMedia {
    fn speech(&self, request: &SpeechRequest<'_>) -> Result<PreparedAudio, MediaError> {
        Media.speech(request)
    }
}
impl NativeMediaPreparer for WarningMedia {
    fn station(&self, _: &str, _: u64) -> Result<Box<dyn StationMediaSession>, MediaError> {
        Ok(Box::new(Self {
            messages: self.messages.clone(),
            reject_end_warning: self.reject_end_warning,
        }))
    }
}
impl StationMediaSession for WarningMedia {
    fn register_program(
        &mut self,
        program: crate::media::MediaProgram,
        one_shot: bool,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        if one_shot {
            for speech in program
                .primary
                .iter()
                .filter_map(|source| source.speech.as_ref())
            {
                if speech.text.contains("Handiham Radio Club Net in ") {
                    self.messages.lock().unwrap().push(speech.text.clone());
                    if self.reject_end_warning && speech.text.contains("Disconnecting from") {
                        return Err(MediaError::Unavailable);
                    }
                }
            }
        }
        TestStation(None).register_program(program, one_shot)
    }
    fn register_prepared(
        &mut self,
        audio: PreparedAudio,
    ) -> Result<Box<dyn crate::audio::PcmStreamReader>, MediaError> {
        TestStation(None).register_prepared(audio)
    }
    fn start(&mut self) -> Result<(), MediaError> {
        Ok(())
    }
}

fn active(rt: &mut Runtime) -> Vec<String> {
    rt.node(PORT)
        .unwrap()
        .links()
        .manager()
        .snapshot()
        .into_iter()
        .filter(|p| !p.ended && !p.retrying)
        .map(|p| p.name)
        .collect()
}

#[test]
fn warning_admission_and_delayed_dial_do_not_keep_coffee_connected() {
    use crate::command_language::MacroOutcome::{Completed, Rejected};
    let mut middle_offers = Vec::new();
    for reject_warning in [false, true] {
        for handi_answers in [false, true] {
            let media = WarningMedia {
                messages: Default::default(),
                reject_end_warning: reject_warning,
            };
            let mut rt = station_fixture::start(
                warning_configuration(),
                &media,
                adapter,
                |_, _| Ok(Box::new(Device(Default::default())) as Box<dyn DeviceHandoff>),
                at(9, 29, 57, 0),
            )
            .unwrap();
            let (mut rx, mut tx) = rt.node(PORT).unwrap().register_audio().unwrap();
            let mut generation = rx.acquire_pair(&mut tx).unwrap().id();
            let mut refresh = false;
            let mut dials = Vec::new();
            let mut outcomes = Vec::new();
            let mut detach_time = None;
            let mut handi_offer = None;
            let mut peers_before_answer = None;
            let mut primary_offers_during_handi = 0;
            let mut snapshots = Vec::new();
            let mut last_second = None;
            for (hour, minute, second) in [
                (9, 29, 57),
                (9, 30, 0),
                (10, 59, 0),
                (11, 0, 0),
                (11, 59, 57),
                (12, 0, 0),
            ] {
                // The link owner runs once per observed wall second. At 11:00 a late
                // first tick lets the next second fall between the end and start macros.
                let first_ms = if (hour, minute) == (11, 0) { 960 } else { 0 };
                let turns = if (hour, minute) == (10, 59) { 3000 } else { 70 };
                for turn in 0..turns {
                    let ms: u16 = first_ms + turn * 20;
                    let now = at(hour, minute, second + (ms / 1000) as u8, ms % 1000);
                    let schedule = last_second.replace(now.wall_seconds) != Some(now.wall_seconds);
                    if schedule {
                        rt.tick_links(now);
                    }
                    if let Some((effect, receipt)) = rt.advance_commands(PORT, now).unwrap() {
                        match &effect {
                            LinkEffect::Detach(peers) if peers.iter().any(|p| p == "51018") => {
                                assert!(detach_time.replace(now.now_ms).is_none());
                            }
                            LinkEffect::Connect(dial) if dial.remote() == "2627" => {
                                assert!(handi_offer.replace(now.now_ms).is_none());
                            }
                            _ => {}
                        }
                        refresh |= accept_effect(&mut rt, effect, now, &mut dials, 100, "macro");
                        receipt.settle(true);
                    }
                    if let Some(done) = rt.node(PORT).unwrap().take_command_completion() {
                        outcomes.push((hour, minute, done.outcome));
                    }
                    if schedule {
                        while let Some((owner, effect)) = rt.next_link() {
                            assert_eq!(owner, PORT);
                            if hour == 11
                                && matches!(&effect, LinkEffect::Connect(d) if d.remote() == "506315")
                            {
                                primary_offers_during_handi += 1;
                            }
                            refresh |=
                                accept_effect(&mut rt, effect, now, &mut dials, 100, "route");
                        }
                    }
                    while dials.first().is_some_and(|(due, _)| *due <= now.now_ms) {
                        let (_, dial) = dials.remove(0);
                        let answered = if dial.remote() == "2627" {
                            peers_before_answer = Some(active(&mut rt));
                            handi_answers
                        } else {
                            true
                        };
                        let middle_primary = hour == 11 && dial.remote() == "506315";
                        let result = rt.finish_connect(PORT, dial, answered, now);
                        if !middle_primary {
                            assert_eq!(result, Ok(answered));
                        }
                        refresh |= result == Ok(true);
                    }
                    if refresh {
                        let prepared = adapter(PORT, rt.settings(PORT).unwrap()).unwrap();
                        match rt.replace_adapter(PORT, prepared, now.now_ms) {
                            Ok(new) => {
                                assert_eq!(rx.acquire_pair(&mut tx).unwrap().id(), new);
                                rt.detached(PORT, generation);
                                rt.reclaim();
                                generation = new;
                                refresh = false;
                            }
                            Err(RuntimeError::Busy) => {}
                            Err(error) => panic!("peer audio refresh: {error:?}"),
                        }
                    }
                }
                snapshots.push(active(&mut rt));
            }
            assert!(dials.is_empty() && !refresh);
            drop((rx, tx));
            let end = at(12, 0, 3, 0).now_ms;
            rt.stop(end);
            for (owner, effect) in rt.take_effects() {
                if let LinkEffect::Detach(peers) = effect {
                    for peer in peers {
                        rt.peer_detached(&owner, &peer, end);
                    }
                }
            }
            rt.detached(PORT, generation);
            assert!(rt.stop(end));
            let messages = media.messages.lock().unwrap();
            eprintln!(
                "COFFEE_WARNINGS reject_warning={reject_warning} handi_answers={handi_answers} detach={detach_time:?} offer={handi_offer:?} before_answer={peers_before_answer:?} held_primary_offers={primary_offers_during_handi} outcomes={outcomes:?} messages={messages:?} snapshots={snapshots:?}"
            );
            assert_eq!(messages.len(), 2);
            for (text, start) in messages.iter().zip(["Connecting to", "Disconnecting from"]) {
                let prefix = format!("{start} Handiham Radio Club Net in ");
                let duration = text.strip_prefix(&prefix).expect("rendered named warning");
                assert!(!duration.is_empty() && !duration.contains('$'));
            }
            assert_eq!(
                outcomes,
                [
                    (9, 30, Completed),
                    (10, 59, Completed),
                    (11, 0, Completed),
                    (11, 0, if reject_warning { Rejected } else { Completed }),
                    (11, 0, Completed),
                    (12, 0, Completed)
                ]
            );
            assert!(matches!((detach_time, handi_offer), (Some(end), Some(start)) if end < start));
            assert_eq!(peers_before_answer, Some(Vec::new()));
            middle_offers.push(primary_offers_during_handi);
            assert_eq!(snapshots[0], ["506315"]);
            assert_eq!(snapshots[1], ["51018"]);
            assert_eq!(snapshots[2], ["51018"]);
            let handi_peers: Vec<&str> = if handi_answers { vec!["2627"] } else { vec![] };
            assert_eq!(snapshots[3], handi_peers);
            assert_eq!(snapshots[4], handi_peers);
            assert_eq!(snapshots[5], ["506315"]);
        }
    }
    assert_eq!(
        middle_offers, [0; 4],
        "the Blind Hams route must stay held across the intervening warning and Handi start"
    );
}
