//! Measured allocation/layout contract, outside production allocator behavior.
use rptadv_peer_support::sip::{
    Header, MAX_FIELD, MAX_MESSAGE, Message, NameAddr, Parser, SipUri, TcpDecoder, Transport,
    Values,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counted;
thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
fn counted() {
    if ACTIVE.try_with(Cell::get).unwrap_or(false) {
        ALLOCATIONS.with(|count| count.set(count.get() + 1));
    }
}
// SAFETY: Every allocation operation delegates unchanged to the system allocator.
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        counted();
        // SAFETY: The caller supplies GlobalAlloc's layout contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        counted();
        // SAFETY: The caller supplies GlobalAlloc's layout contract.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        counted();
        // SAFETY: The caller supplies the original system allocation and layout.
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller supplies the original system allocation and layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn inspect(message: &Message<'_>, scratch: &mut [u8; MAX_FIELD]) {
    std::hint::black_box(message.start_line());
    std::hint::black_box(message.body());
    let _ = std::hint::black_box(message.cseq(scratch));
    let _ = std::hint::black_box(message.call_id(scratch));
    for header in message.headers() {
        std::hint::black_box(header.name());
        std::hint::black_box(header.raw_value());
        let _ = std::hint::black_box(header.name_addr(scratch));
        for value in header.values(scratch).flatten() {
            if let Ok(address) = NameAddr::parse(value) {
                if let Ok(uri) = SipUri::parse(address.uri()) {
                    let _ = std::hint::black_box(uri.require_cleartext_routing());
                    std::hint::black_box(uri.parameter(b"transport"));
                }
            }
        }
    }
}

#[test]
fn parse_paths_allocate_zero_and_retain_only_fixed_workspace() {
    let sample = include_bytes!("fixtures/sip/rfc4475-wsinv.sip");
    let mut parser = Parser::default();
    let mut storage = [0; MAX_MESSAGE];
    let mut decoder = TcpDecoder::new(&mut storage);
    let mut scratch = [0; MAX_FIELD];
    let mut mutation = [0; MAX_MESSAGE];
    mutation[..sample.len()].copy_from_slice(sample);
    ALLOCATIONS.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
    for _ in 0..32 {
        inspect(&parser.parse(sample, Transport::Udp).unwrap(), &mut scratch);
        for byte in sample {
            decoder.push(&[*byte]).unwrap();
        }
        inspect(&decoder.message().unwrap(), &mut scratch);
        decoder.reset();
    }
    // Additional independent fixtures and review regressions exercise the
    // safe typed raw-header entry and newly bounded reason/parameter paths.
    for wire in [
        include_bytes!("fixtures/sip/rfc4475-lwsdisp.sip").as_slice(),
        include_bytes!("fixtures/sip/rfc4475-unreason.sip").as_slice(),
        include_bytes!("fixtures/sip/rfc5118-ipv6-good.sip").as_slice(),
        include_bytes!("fixtures/sip/rfc5118-ipv6-bad.sip").as_slice(),
        b"SIP/2.0 200 OK\r\nFrom: <sip:a@b>;tag=one;TAG=two\r\nl:0\r\n\r\n",
        b"SIP/2.0 200 OK\r\nFrom: <sip:a@b>;p=\"bad\\\r\n fold\"\r\nl:0\r\n\r\n",
    ] {
        inspect(&parser.parse(wire, Transport::Udp).unwrap(), &mut scratch);
        decoder.reset();
        for byte in wire {
            decoder.push(&[*byte]).unwrap();
        }
        inspect(&decoder.message().unwrap(), &mut scratch);
    }
    // Exercise malformed byte paths, not only a friendly generated request.
    for at in 0..sample.len() {
        let original = mutation[at];
        for byte in [0, b'\r', b'\n', b'"', b'\\', b'%', b'@', 0xff] {
            mutation[at] = byte;
            if let Ok(message) = parser.parse(&mutation[..sample.len()], Transport::Udp) {
                inspect(&message, &mut scratch);
            }
            decoder.reset();
            if decoder.push(&mutation[..sample.len()]).is_ok() {
                if let Some(message) = decoder.message() {
                    inspect(&message, &mut scratch);
                }
            }
        }
        mutation[at] = original;
    }
    for target in [
        b"sips:a@host;transport=tcp".as_slice(),
        b"sip:user%00:secret@[2001:db8::1]:5060;transport=udp?subject=x",
        b"sip:a;transport=tcp;Transport=udp",
        b"sip:a;%74ransport=tls",
        b"sip:a;TRANS%70ORT=%74ls",
        b"sip:a;p/=one;p%2f=two",
        b"sip:a;p%2f=one;p%252f=two;p/=three",
    ] {
        if let Ok(uri) = std::hint::black_box(SipUri::parse(target)) {
            let _ = std::hint::black_box(uri.require_cleartext_routing());
            std::hint::black_box(uri.parameter(b"transport"));
        }
    }
    ACTIVE.with(|active| active.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    let parser_bytes = std::mem::size_of::<Parser>();
    let tcp_bytes = std::mem::size_of::<TcpDecoder>();
    let view_bytes = std::mem::size_of::<Message>();
    let uri_bytes = std::mem::size_of::<SipUri>();
    let header_bytes = std::mem::size_of::<Header>();
    let values_bytes = std::mem::size_of::<Values>();
    let address_bytes = std::mem::size_of::<NameAddr>();
    let views = view_bytes + uri_bytes + header_bytes + values_bytes + address_bytes;
    assert!(tcp_bytes + MAX_FIELD + views <= 8192);
    assert!(parser_bytes + MAX_FIELD + views <= 8192);
    eprintln!(
        "parser={parser_bytes} tcp_decoder={tcp_bytes} scratch={MAX_FIELD} message_view={view_bytes} uri_view={uri_bytes} header_view={header_bytes} values_view={values_bytes} address_view={address_bytes} fixed_wire={MAX_MESSAGE} allocations=0"
    );
}
