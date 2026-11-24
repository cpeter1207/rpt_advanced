//! Product and native SIP interop with only the physical RF edge under fixture control.
use super::*;
use super::{phone_caller::Caller, phone_owner::Owners, private_phone::{register, registrar}};
use std::net::UdpSocket;

struct Running {
    owners: Option<Box<Owners>>,
    table: Option<Box<crate::abi::rptadv_host_services_v10>>,
    binding: Option<Box<crate::abi::rptadv_station_binding_v1>>,
    active: bool,
}
impl Running {
    fn start(document: &[u8]) -> Self {
        let mut owners=Owners::new();
        let binding=Box::new(ops());
        let table=Box::new(owners.table(&binding));
        let manage=unsafe {&*crate::lifecycle::bootstrap::rptadv_product_management_descriptor_v1()};
        let mut running=Self { owners:Some(owners), binding:Some(binding), table:Some(table), active:false };
        let status=unsafe {(manage.start.unwrap())(running.table.as_deref().unwrap(),fixture::control_descriptor().cast(),
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),document.as_ptr().cast(),document.len())};
        running.active=matches!(status,0|-2);
        assert_eq!(status,0);
        running
    }
    fn stop(&mut self) -> bool {
        let product=unsafe {&*crate::lifecycle::rptadv_product_descriptor_v1()};
        let until=Instant::now()+Duration::from_secs(8);
        while Instant::now()<until {
            if self.active { self.active = unsafe { product.stop.unwrap()() } != 0; }
            if !self.active && self.owners.as_ref().unwrap().joined() { return true; }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.stop() {
            // This opt-in fixture runs in its own child. An uncertain native owner
            // retains the borrowed descriptors even during assertion unwinding.
            let _=self.owners.take().map(Box::leak);
            let _=self.table.take().map(Box::leak);
            let _=self.binding.take().map(Box::leak);
        }
    }
}

pub(in super::super) fn positive_phone(scenario: &Scenario<'_>) {
    let _serial=fixture::LIFECYCLE.lock().unwrap_or_else(|e|e.into_inner());
    fixture::REVIEW_CARRIER.store(false,Ordering::Release);
    let (document,address)=provisioning(scenario);
    let mut managed=Running::start(&serde_json::to_vec(&document).unwrap());
    let client=Client { pki:scenario.client.pki, port:address.port(), proxy:None };
    client.ready();
    let path=edit(&client,"POST",PORTS,json!({"key":"call","configuration":{"name":"Call","inherit_from":"site","enabled":false}})).headers["location"].clone();
    let port=path.trim_end_matches('/').rsplit('/').next().unwrap();
    let id=port.parse().unwrap();
    let station=&managed.owners.as_ref().unwrap().station;
    *station.port.lock().unwrap()=Some(id);
    station.allow_open.store(true,Ordering::Release);
    let output=fixture::watch_output(port);
    output.measure_without_carrier.store(true,Ordering::Release);
    fixture::WATCH_PHONE_PCM.store(true,Ordering::Release);
    put(&client,&format!("{path}interface/"),json!({"audio":{"device_identifier":"hw:fixture"}}));
    put(&client,&format!("{path}radio/"),json!({"duplex_mode":"full"}));
    let pin="$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";
    let mailbox=edit(&client,"POST",&format!("{path}mailboxes/"),json!({"key":"741","configuration":{
        "first_name":"Test","callsign":"N0TEST","pin_hash":pin}})).headers["location"].clone();
    assert!(!client.get(&mailbox).body.to_string().contains(pin));
    if std::env::var(CASE).as_deref()==Ok("native-private-managed-iax") {
        super::phone_iax::positive_iax(&client,&path,&mailbox,managed.owners.as_ref().unwrap(),&output);
        assert!(managed.stop());
        return;
    }
    let socket=UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(12))).unwrap();
    let account=edit(&client,"POST",&format!("{path}protocols/phone-patch/"),json!({"configuration":{
        "provider":{"protocol":"sip","server":"127.0.0.1","port":socket.local_addr().unwrap().port(),"username":"desk"},
        "credentials":{"password":"fixture-only"},"vad":{"enabled":false}}})).headers["location"].clone();
    let contact=thread::scope(|scope| {
        let network=scope.spawn(||{
            let (validated,remote)=register(&socket);
            socket.send_to(registrar::response(&validated,200,false).as_bytes(),remote).unwrap();
            remote
        });
        put(&client,&path,json!({"name":"Call","inherit_from":"site","enabled":true}));
        network.join().unwrap()
    });
    wait_until(||output.value.load(Ordering::Acquire)==0);
    let keys=output.keyed.load(Ordering::Acquire);
    let audible=output.audible.load(Ordering::Acquire);
    let inband=std::env::var(CASE).as_deref()==Ok("native-private-inband");
    let mut caller=Caller::connect(&socket,contact,1,!inband);
    assert!(caller.receive_private_audio()>0);
    caller.digits("741#0000#");
    caller.audio(8,false);
    assert_eq!(output.keyed.load(Ordering::Acquire),keys,"caller keyed before mailbox login");
    assert_eq!(output.audible.load(Ordering::Acquire),audible,"private prompt escaped to RF");
    caller.digits("741#1234#");
    caller.digits("*3#");
    caller.audio(18,false);
    assert!(output.keyed.load(Ordering::Acquire)>keys,"authorized attachment never keyed callback");
    let counters = [&fixture::PHONE_ORIGINAL, &fixture::PHONE_FRESH, &fixture::PHONE_PERMITTED, &fixture::PHONE_ACCEPTED]
        .map(|count| count.load(Ordering::Acquire));
    eprintln!("native PCMU nonzero samples original/fresh/permitted/accepted {counters:?}, callback keys {} audible {}",
        output.keyed.load(Ordering::Acquire), output.audible.load(Ordering::Acquire));
    assert!(counters[3] > 0, "no authentic nonzero caller PCM was credited to the private RF route: {counters:?}");
    assert!(output.audible.load(Ordering::Acquire)>audible,"actual PCMU never reached station callback");
    caller.digits("*99#");
    wait_until(||output.value.load(Ordering::Acquire)==0);
    let muted=output.keyed.load(Ordering::Acquire);
    let quiet=output.audible.load(Ordering::Acquire);
    caller.audio(18,false);
    assert_eq!(output.keyed.load(Ordering::Acquire),muted);
    assert_eq!(output.audible.load(Ordering::Acquire),quiet);
    caller.digits("*99#");
    caller.audio(18,false);
    assert!(output.keyed.load(Ordering::Acquire)>muted,"muted caller lost its digit path");
    assert!(output.audible.load(Ordering::Acquire)>quiet);
    let revoked = if inband {
        eprintln!("PCMU-only SIP: scoped mailbox login, attach, voice, mute and unmute reached the station callback");
        caller
    } else {
    let owner=managed.owners.as_ref().unwrap();
    let identity=owner.read_identity();
    assert_eq!(identity.source,socket.local_addr().unwrap());
    assert_ne!(identity.clock_epoch,0);
    let media = || [
        owner.read_count.load(Ordering::Acquire),
        fixture::PHONE_ORIGINAL.load(Ordering::Acquire),
        fixture::PHONE_FRESH.load(Ordering::Acquire),
        fixture::PHONE_PERMITTED.load(Ordering::Acquire),
        fixture::PHONE_ACCEPTED.load(Ordering::Acquire),
        output.keyed.load(Ordering::Acquire),
        output.dekeyed.load(Ordering::Acquire),
        output.audible.load(Ordering::Acquire),
        fixture::PHONE_LIVE_SAMPLES.load(Ordering::Acquire),
        fixture::PHONE_GAP_CREDIT.load(Ordering::Acquire),
        fixture::PHONE_ROUTE_INSTALLS.load(Ordering::Acquire),
    ];
    if std::env::var(CASE).as_deref()==Ok("native-private-retained-vad") {
        caller.during(false, || edit(&client,"PATCH",&account,json!([{"op":"replace","path":"/configuration/vad/enabled","value":true}])));
        assert_eq!(owner.read_identity(),identity);
        // Real packets from before the edit may still emerge. Drive genuine silent
        // RTP past the last nonzero sample and the documented 1000 ms speech hang,
        // 250 ms playout target and 500 ms maximum age of this profile.
        let mut last_nonzero=fixture::PHONE_ORIGINAL.load(Ordering::Acquire);
        let mut silent_packets=0;
        let tail=(1000_usize+250+500).div_ceil(20);
        for _ in 0..2*tail {
            caller.audio(1,true);
            let current=fixture::PHONE_ORIGINAL.load(Ordering::Acquire);
            if current==last_nonzero {silent_packets+=1;} else {silent_packets=0;last_nonzero=current;}
            if silent_packets>=tail {break;}
        }
        assert_eq!(silent_packets,tail,"the observed SIP source never finished delivering its earlier real speech");
        let quiet_keys=output.keyed.load(Ordering::Acquire);
        let prior=media();
        caller.audio(24,true);
        let after=media();
        eprintln!("retained silent RTP read/original/fresh/permitted/credited/key/dekey/audible/live/gap/route {prior:?} -> {after:?}");
        assert!(after[0]>prior[0] && after[8]>prior[8],"fresh genuine silent RTP lost across policy edit");
        assert_eq!(after[1],prior[1],"prior programme contaminated the silent observation");
        assert_eq!(owner.read_identity(),identity);
        assert_eq!(output.keyed.load(Ordering::Acquire),quiet_keys,
            "retained dialog must adopt newly enabled speech qualification");
        assert_eq!(output.value.load(Ordering::Acquire),0);
        caller.spoken_yes();
        caller.audio(12,true);
        assert!(output.keyed.load(Ordering::Acquire)>quiet_keys,
            "fresh spoken PCMU did not qualify the same VAD-enabled native call");
        let voiced_keys=output.keyed.load(Ordering::Acquire);
        caller.audio(tail,true);
        assert_eq!(output.value.load(Ordering::Acquire),0,
            "genuine speech hang must expire on continued silence");
        let expired_voice=output.keyed.load(Ordering::Acquire);
        caller.audio(24,true);
        assert_eq!(output.keyed.load(Ordering::Acquire),expired_voice,
            "an older positive classifier position renewed the expired voice hang");
        eprintln!("retained VAD real speech keyed {voiced_keys}, expired on fresh silent RTP at {expired_voice}");
        caller.during(true, || edit(&client,"PATCH",&account,json!([{"op":"replace","path":"/configuration/vad/enabled","value":false}])));
        caller.audio(24,true);
        assert!(output.keyed.load(Ordering::Acquire)>expired_voice,
            "disabling speech qualification must restore live silent-speech admission on the same dialog");
        assert_eq!(owner.read_identity(),identity);
        caller.during(false, || edit(&client,"PATCH",&account,json!([
            {"op":"add","path":"/configuration/timing","value":{"target_latency_ms":1,"maximum_latency_ms":1}},
            {"op":"add","path":"/configuration/isolate_peers","value":false}
        ])));
        wait_until(||output.value.load(Ordering::Acquire)==0);
        caller.resume_audio_clock();
        let expired=output.keyed.load(Ordering::Acquire);
        caller.audio(24,false);
        assert_eq!(output.keyed.load(Ordering::Acquire),expired,
            "actual jitter-held RTP older than one millisecond must not open the retained route");
        assert_eq!(owner.read_identity(),identity);
        caller.during(false, || edit(&client,"PATCH",&account,json!([
            {"op":"replace","path":"/configuration/timing","value":{"target_latency_ms":250,"maximum_latency_ms":500}},
            {"op":"replace","path":"/configuration/isolate_peers","value":true}
        ])));
        caller.audio(24,false);
        assert!(output.keyed.load(Ordering::Acquire)>expired);
        assert_eq!(owner.read_identity(),identity);
        eprintln!("retained native media owner {:?}, SIP contact {contact}, qualified VAD and 1/500ms cutoffs",identity);
    }
    let before_edit=media();
    caller.during(false, || edit(&client,"POST",PORTS,json!({"key":"other","configuration":{
        "name":"Other","inherit_from":"site","enabled":false}})));
    let after_edit=media();
    caller.audio(18,false);
    let resumed=media();
    eprintln!("station edit read/original/fresh/permitted/credited/key/dekey/audible/live/gap/route {before_edit:?} -> {after_edit:?} -> {resumed:?}; source {:?}", owner.read_identity());
    assert!(resumed[0]>after_edit[0],"the established native owner stopped reading after unrelated adoption");
    assert_eq!(owner.read_identity(),identity);
    assert!(resumed[7]>after_edit[7],"unrelated adoption lost the established call");
    caller.hangup();
    wait_until(||output.value.load(Ordering::Acquire)==0);
    eprintln!("starting negotiated PCMU-only managed dialog");
    let mut second=Caller::connect(&socket,contact,2,false);
    second.digits("741#1234#");
    second.digits("*3#");
    let second_before=output.audible.load(Ordering::Acquire);
    second.audio(18,false);
    assert!(output.audible.load(Ordering::Acquire)>second_before);
    second
    };
    thread::scope(|scope| {
        let withdrawn_call=scope.spawn(move ||revoked.expect_revocation());
        assert_eq!(client.edit("DELETE",&mailbox,None).code,202);
        client.ready();
        withdrawn_call.join().unwrap();
    });
    wait_until(||output.value.load(Ordering::Acquire)==0);
    thread::scope(|scope| {
        let withdrawn=scope.spawn(||{
            let mut bytes=[0;8192];
            let (message,remote)=registrar::receive_register(&socket,&mut bytes).unwrap();
            assert_eq!(remote,contact);
            assert!(message.starts_with("REGISTER "));
            assert_eq!(registrar::header(&message,"Expires"),"0");
            socket.send_to(registrar::response(&message,200,false).as_bytes(),remote).unwrap();
        });
        put(&client,&path,json!({"name":"Call","inherit_from":"site","enabled":false}));
        withdrawn.join().unwrap();
    });
    eprintln!("two real dialogs on {contact}, private prompt delivered, station callback keys {} audible {}, grant revoked and native account withdrawn",
        output.keyed.load(Ordering::Acquire),output.audible.load(Ordering::Acquire));
    assert!(managed.stop());
}
