//! Deterministic host symbols using the actual generated public-header layouts.
#![allow(unsafe_op_in_unsafe_fn)]
use crate::bindings::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    ffi::{CStr, c_char, c_void},
    ptr,
};

thread_local! { static FAIL_ALLOCATION_BYTES: Cell<usize> = const { Cell::new(0) }; }
pub static FAILED_READY_CHANNEL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
pub static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Allocator;
fn reject_allocation(bytes: usize) -> bool {
    FAIL_ALLOCATION_BYTES
        .try_with(|selected| {
            if selected.get() == bytes {
                selected.set(0);
                true
            } else {
                false
            }
        })
        .unwrap_or(false)
}
// SAFETY: forwards unchanged layouts and pointers to System; the test-only thread-local
// switch rejects one selected allocation by returning null, as GlobalAlloc permits.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if reject_allocation(layout.size()) {
            ptr::null_mut()
        } else {
            System.alloc(layout)
        }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        if reject_allocation(bytes) {
            ptr::null_mut()
        } else {
            System.realloc(pointer, layout, bytes)
        }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

pub fn fail_allocation<T>(bytes: usize, operation: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            FAIL_ALLOCATION_BYTES.set(0);
        }
    }
    assert_ne!(bytes, 0);
    assert_eq!(FAIL_ALLOCATION_BYTES.replace(bytes), 0);
    let _clear = Clear;
    let result = operation();
    assert_eq!(
        FAIL_ALLOCATION_BYTES.get(),
        0,
        "selected allocation was not reached"
    );
    result
}

#[unsafe(no_mangle)]
unsafe extern "C" fn ast_call(
    _: *mut ast_channel,
    destination: *const c_char,
    timeout: i32,
) -> i32 {
    assert_eq!(CStr::from_ptr(destination), c"usb");
    assert_eq!(timeout, 0);
    host(|s| {
        s.calls += 1;
        if s.failure == 30 { -1 } else { 0 }
    })
}

pub struct Pending {
    frame: Box<ast_frame>,
    _samples: Vec<i16>,
}

pub struct State {
    pub calls: usize,
    pub failure: u8,
    pub native: usize,
    pub missing_linear: bool,
    pub missing_cache: Option<usize>,
    pub blocked: Option<(usize, bool)>,
    pub duplicate: bool,
    pub technology_unloaded: bool,
    pub rates: Box<[u32; 8]>,
    pub codecs: Box<[ast_codec; 8]>,
    tech: Box<ast_channel_tech>,
    tokens: Box<[u64; 4]>,
    pub refs: HashMap<usize, i32>,
    pub channels: usize,
    pub read_rates: HashMap<usize, u32>,
    pub freed: usize,
    pub writes: Vec<Vec<i16>>,
    pub write_types: Vec<u32>,
    pub indications: Vec<i32>,
    pub offers: Vec<usize>,
    pub translators: usize,
    pub buffered: bool,
    queue: VecDeque<Pending>,
    active: HashMap<usize, Pending>,
}

impl Default for State {
    fn default() -> Self {
        let rates = Box::new([48000, 16000, 16000, 8000, 96000, 44100, 0, 16000]);
        let mut codecs: Box<[ast_codec; 8]> = Box::new(unsafe { std::mem::zeroed() });
        for (index, codec) in codecs.iter_mut().enumerate() {
            codec.id = index as u32 + 1;
            codec.type_ = AST_MEDIA_TYPE_AUDIO;
            codec.sample_rate = rates[index];
        }
        Self {
            calls: 0,
            failure: 0,
            native: 0,
            missing_linear: false,
            missing_cache: None,
            blocked: None,
            duplicate: false,
            technology_unloaded: false,
            rates,
            codecs,
            tech: Box::new(unsafe { std::mem::zeroed() }),
            tokens: Box::new([0; 4]),
            refs: HashMap::new(),
            channels: 0,
            read_rates: HashMap::new(),
            freed: 0,
            writes: vec![],
            write_types: vec![],
            indications: vec![],
            offers: vec![],
            translators: 0,
            buffered: false,
            queue: VecDeque::new(),
            active: HashMap::new(),
        }
    }
}

impl State {
    pub fn format(&mut self, index: usize) -> *mut ast_format {
        (&mut self.rates[index] as *mut u32).cast()
    }
    pub(crate) fn token<T>(&mut self, index: usize) -> *mut T {
        (&mut self.tokens[index] as *mut u64).cast()
    }
    fn reference<T>(&mut self, value: *mut T) -> *mut T {
        *self.refs.entry(value as usize).or_default() += 1;
        value
    }
    pub fn clean(&self) {
        assert!(
            self.refs.values().all(|count| *count == 0),
            "{:?}",
            self.refs
        );
        assert_eq!(self.channels, 0);
        assert_eq!(self.translators, 0);
        assert!(self.active.is_empty());
    }
    pub fn voice(&mut self, samples: Vec<i16>, malformed: u8) {
        let mut samples = samples;
        let mut frame: Box<ast_frame> = Box::new(unsafe { std::mem::zeroed() });
        frame.frametype = AST_FRAME_VOICE;
        frame.samples = samples.len() as i32;
        frame.datalen = frame.samples * 2;
        frame.subclass.__bindgen_anon_1.format = self.format(0);
        frame.data.ptr = samples.as_mut_ptr().cast();
        match malformed {
            1 => frame.data.ptr = ptr::null_mut(),
            2 => frame.samples = 0,
            3 => frame.datalen = 0,
            4 => frame.datalen -= 1,
            5 => frame.subclass.__bindgen_anon_1.format = self.format(1),
            6 => frame.samples = -1,
            _ => (),
        }
        self.queue.push_back(Pending {
            frame,
            _samples: samples,
        });
    }
    pub fn control(&mut self, condition: i32) {
        let mut frame: Box<ast_frame> = Box::new(unsafe { std::mem::zeroed() });
        frame.frametype = AST_FRAME_CONTROL;
        frame.subclass.integer = condition;
        self.queue.push_back(Pending {
            frame,
            _samples: vec![],
        });
    }
    pub fn edit_frame(&mut self, edit: impl FnOnce(&mut ast_frame)) {
        edit(&mut self.queue.back_mut().expect("queued fixture frame").frame);
    }
}

thread_local! { static HOST: RefCell<State> = RefCell::new(State::default()); }
pub fn host<R>(f: impl FnOnce(&mut State) -> R) -> R {
    HOST.with(|s| f(&mut s.borrow_mut()))
}
pub fn reset() {
    host(|s| {
        s.clean();
        *s = State::default();
    });
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __ao2_ref(
    value: *mut c_void,
    delta: i32,
    _: *const c_char,
    _: *const c_char,
    _: i32,
    _: *const c_char,
) -> i32 {
    host(|s| {
        // Cache getters lend untracked references; a positive AO2 ref establishes
        // the explicit owner that this fixture must later observe released.
        let count = s.refs.entry(value as usize).or_default();
        *count += delta;
        assert!(*count >= 0);
        *count
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_get_channel_tech(name: *const c_char) -> *const ast_channel_tech {
    assert_eq!(CStr::from_ptr(name), c"RadioPlusAdvanced");
    host(|s| {
        s.tech.capabilities = s.token(1);
        let technology = (&*s.tech) as *const _;
        if s.technology_unloaded {
            s.tech.capabilities = ptr::null_mut();
        }
        if s.failure == 1 {
            ptr::null()
        } else {
            technology
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_cap_get_format(
    cap: *const ast_format_cap,
    index: i32,
) -> *mut ast_format {
    assert_eq!(index, 0);
    host(|s| {
        assert_eq!(cap, s.token(3));
        assert_eq!(s.channels, 1);
        if s.failure == 2 || s.failure == 26 {
            return ptr::null_mut();
        }
        let f = s.format(s.native);
        s.reference(f)
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_channel_nativeformats(_: *const ast_channel) -> *mut ast_format_cap {
    host(|s| {
        assert_eq!(s.channels, 1);
        if s.failure == 23 {
            ptr::null_mut()
        } else {
            s.token(3)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_cap_count(cap: *const ast_format_cap) -> usize {
    host(|s| {
        assert_eq!(cap, s.token(3));
        match s.failure {
            24 => 0,
            25 => 2,
            _ => 1,
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_cache_get_slin_by_rate(rate: u32) -> *mut ast_format {
    host(|s| {
        if s.failure == 2 || s.missing_linear {
            ptr::null_mut()
        } else {
            s.format(if s.failure == 3 {
                1
            } else {
                match rate {
                    16000 => 2,
                    8000 => 3,
                    _ => 0,
                }
            })
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_get_sample_rate(format: *const ast_format) -> u32 {
    *format.cast::<u32>()
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_cmp(
    a: *const ast_format,
    b: *const ast_format,
) -> ast_format_cmp_res {
    if a == b {
        AST_FORMAT_CMP_EQUAL
    } else {
        AST_FORMAT_CMP_NOT_EQUAL
    }
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_request(
    kind: *const c_char,
    cap: *mut ast_format_cap,
    ids: *const ast_assigned_ids,
    requestor: *const ast_channel,
    name: *const c_char,
    cause: *mut i32,
) -> *mut ast_channel {
    assert_eq!(CStr::from_ptr(kind), c"RadioPlusAdvanced");
    assert_eq!(CStr::from_ptr(name), c"usb");
    assert!(ids.is_null() && requestor.is_null());
    *cause = 0;
    host(|s| {
        assert_eq!(cap, s.token(2));
        if s.failure == 4 {
            ptr::null_mut()
        } else {
            s.channels += 1;
            s.token(0)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_hangup(_: *mut ast_channel) {
    host(|s| {
        assert!(s.channels > 0);
        s.channels -= 1;
    });
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_set_read_format(
    channel: *mut ast_channel,
    format: *mut ast_format,
) -> i32 {
    host(|s| {
        if s.failure == 5 {
            -1
        } else {
            s.read_rates.insert(channel as usize, *format.cast::<u32>());
            0
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_set_write_format(_: *mut ast_channel, _: *mut ast_format) -> i32 {
    host(|s| if s.failure == 6 { -1 } else { 0 })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_codec_get_max() -> i32 {
    host(|s| if s.failure == 10 { -1 } else { 9 })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_codec_get_by_id(id: i32) -> *mut ast_codec {
    host(|s| {
        if id == 9 {
            ptr::null_mut()
        } else {
            let p = &mut s.codecs[id as usize - 1] as *mut _;
            s.reference(p)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_format_cache_get_by_codec(codec: *const ast_codec) -> *mut ast_format {
    let index = (*codec).id as usize - 1;
    host(|s| {
        if s.missing_cache == Some(index) {
            ptr::null_mut()
        } else {
            let f = s.format(if s.failure == 32 && index == 1 {
                3
            } else if s.duplicate && index == 7 {
                2
            } else {
                index
            });
            s.reference(f)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_translate_path_steps(
    destination: *mut ast_format,
    source: *mut ast_format,
) -> u32 {
    host(|s| {
        if s.blocked.is_some_and(|(index, dest)| {
            if dest {
                destination == s.format(index)
            } else {
                source == s.format(index)
            }
        }) {
            u32::MAX
        } else {
            1
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn __ast_format_cap_alloc(
    _: ast_format_cap_flags,
    _: *const c_char,
    _: *const c_char,
    _: i32,
    _: *const c_char,
) -> *mut ast_format_cap {
    host(|s| {
        if s.failure == 11 {
            ptr::null_mut()
        } else {
            let cap = s.token(2);
            s.reference(cap)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn __ast_format_cap_append(
    _: *mut ast_format_cap,
    format: *mut ast_format,
    framing: u32,
    _: *const c_char,
    _: *const c_char,
    _: i32,
    _: *const c_char,
) -> i32 {
    assert_eq!(framing, 0);
    host(|s| {
        s.offers.push(format as usize);
        if s.failure == 12 { -1 } else { 0 }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_read(_: *mut ast_channel) -> *mut ast_frame {
    host(|s| {
        let Some(mut pending) = s.queue.pop_front() else {
            return ptr::null_mut();
        };
        let p = &mut *pending.frame as *mut _;
        s.active.insert(p as usize, pending);
        p
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_frame_free(frame: *mut ast_frame, cache: i32) {
    assert_eq!(cache, 1);
    host(|s| {
        assert!(s.active.remove(&(frame as usize)).is_some());
        s.freed += 1;
    });
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_write(_: *mut ast_channel, frame: *mut ast_frame) -> i32 {
    let words = if (*frame).samples == 0 {
        &[]
    } else {
        std::slice::from_raw_parts((*frame).data.ptr.cast::<i16>(), (*frame).samples as usize)
    };
    host(|s| {
        s.write_types.push((*frame).frametype);
        s.writes.push(words.to_vec());
        if s.failure == 8 { -1 } else { 0 }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_indicate(_: *mut ast_channel, condition: i32) -> i32 {
    host(|s| {
        s.indications.push(condition);
        if s.failure == 7 { -1 } else { 0 }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_translator_build_path(
    _: *mut ast_format,
    _: *mut ast_format,
) -> *mut ast_trans_pvt {
    host(|s| {
        if s.failure == 13 {
            ptr::null_mut()
        } else {
            s.translators += 1;
            s.token(3)
        }
    })
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_translator_free_path(_: *mut ast_trans_pvt) {
    host(|s| {
        assert_eq!(s.translators, 1);
        s.translators -= 1;
    });
}
#[unsafe(no_mangle)]
unsafe extern "C" fn ast_translate(
    _: *mut ast_trans_pvt,
    frame: *mut ast_frame,
    consume: i32,
) -> *mut ast_frame {
    assert_eq!(consume, 1);
    if host(|s| s.buffered) {
        ast_frame_free(frame, 1);
        ptr::null_mut()
    } else {
        host(|state| match state.failure {
            35 => {
                (*frame).samples = 0;
                (*frame).datalen = 0;
            }
            36 => (*frame).frametype = AST_FRAME_TEXT,
            37 => (*frame).subclass.__bindgen_anon_1.format = state.format(0),
            _ => (),
        });
        frame
    }
}
