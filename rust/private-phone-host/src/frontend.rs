//! Shared checked C callbacks. The two hosts supply only their actual context
//! and native peer enum conversions; no product ABI pointer crosses between hosts.
use crate::{Error, abi, calls::PrivateCall, phone::{AccountRow,PreparedAccount,Protocol},selection::PhoneHost};
use std::{ffi::c_void, mem::size_of, panic::{AssertUnwindSafe,catch_unwind}, ptr, slice};

/// Implemented by the frontend that owns the process's opaque host and peer types.
///
/// # Safety
/// A live context must be from this frontend and its peer must be inspected as
/// the exact enum created by wrap. Borrowed owners stay live for each operation.
pub unsafe trait Frontend {
    /// # Safety
    /// Context refers to this frontend's retained host table.
    unsafe fn host<'a>(context:*mut c_void)->Option<&'a PhoneHost>;
    /// # Safety
    /// Peer is a live handle previously created by this frontend.
    unsafe fn call<'a>(peer:*mut c_void)->Option<&'a PrivateCall>;
    /// Transfer one owned native call to the frontend peer enum.
    fn wrap(call:PrivateCall)->*mut c_void;
    /// # Safety
    /// This is a unique handle returned by wrap which the product declined to consume.
    unsafe fn discard(peer:*mut c_void);
}

fn boundary(work:impl FnOnce()->Result<i32,Error>)->i32 {
    catch_unwind(AssertUnwindSafe(work)).ok().and_then(Result::ok).unwrap_or(-1)
}
unsafe fn bytes<'a>(input:abi::rptadv_peer_bytes_v1, max:usize)->Result<&'a[u8],Error>{
    if input.length==0 || input.length>max || input.data.is_null(){return Err(Error::Input);}
    Ok(unsafe {slice::from_raw_parts(input.data,input.length)})
}
unsafe fn row(input:&abi::rptadv_phone_account_v1)->Result<PreparedAccount,Error>{
    if input.struct_size as usize!=size_of::<abi::rptadv_phone_account_v1>()
        || input.abi_version!=1 || input.reserved!=0 { return Err(Error::Input); }
    let protocol=match input.protocol{1=>Protocol::Sip,2=>Protocol::Iax2,_=>return Err(Error::Input)};
    let server=std::str::from_utf8(unsafe{bytes(input.server,255)}?).map_err(|_|Error::Input)?;
    let username=std::str::from_utf8(unsafe{bytes(input.username,255)}?).map_err(|_|Error::Input)?;
    PreparedAccount::copy(AccountRow { port_id:input.port_uuid,account_id:input.account_uuid,
        generation:input.generation,protocol,server,port:input.port.try_into().map_err(|_|Error::Input)?,
        username,credential:unsafe{bytes(input.credential,4096)}?,
        jitter_ms:[input.jitter_minimum_ms,input.jitter_initial_ms,input.jitter_maximum_ms],
    }).map_err(|_|Error::Input)
}
unsafe extern "C" fn prepare<F:Frontend>(ctx:*mut c_void,rows:*const abi::rptadv_phone_account_v1,
    count:usize,out:*mut *mut c_void)->i32 {
    boundary(||{
        let out=unsafe{out.as_mut()}.ok_or(Error::Input)?;
        *out=ptr::null_mut();
        let host=unsafe{F::host(ctx)}.ok_or(Error::Input)?;
        let mut owned=Vec::new();
        if count>0 {
            if rows.is_null() || count>isize::MAX as usize /size_of::<abi::rptadv_phone_account_v1>() {return Err(Error::Input);}
            owned.try_reserve_exact(count).map_err(|_|Error::Input)?;
            for input in unsafe {slice::from_raw_parts(rows,count)} {owned.push(unsafe {row(input)}?);}
        }
        let ticket=host.prepare(owned)?;
        *out=ptr::without_provenance_mut(ticket);
        Ok(0)
    })
}
unsafe extern "C" fn drive<F:Frontend,const REVERSE:bool>(ctx:*mut c_void,ticket:*mut c_void)->i32 {
    boundary(|| {
        let host=unsafe{F::host(ctx)}.ok_or(Error::Input)?;
        host.drive(ticket.addr(),REVERSE).map(|done|i32::from(!done))
    })
}
unsafe extern "C" fn release<F:Frontend>(ctx:*mut c_void,ticket:*mut c_void)->i32 {
    boundary(|| {
        unsafe{F::host(ctx)}.ok_or(Error::Input)?.release(ticket.addr())?;
        Ok(0)
    })
}
unsafe extern "C" fn observe<F:Frontend>(ctx:*mut c_void,port:*const u8,id:*const u8,generation:u64,
    out:*mut abi::rptadv_phone_account_status_v1)->i32 {
    boundary(|| {
        if out.is_null() || unsafe{ptr::addr_of!((*out).struct_size).read_unaligned()}<(size_of::<abi::rptadv_phone_account_status_v1>() as u32)
            || unsafe{ptr::addr_of!((*out).abi_version).read_unaligned()}!=1 {return Err(Error::Input);}
        let host=unsafe{F::host(ctx)}.ok_or(Error::Input)?;
        let port=unsafe {port.cast::<[u8;16]>().as_ref()}.ok_or(Error::Input)?;
        let id=unsafe {id.cast::<[u8;16]>().as_ref()}.ok_or(Error::Input)?;
        let state=host.observe(port,id,generation)?;
        unsafe{out.write(abi::rptadv_phone_account_status_v1{struct_size:size_of::<abi::rptadv_phone_account_status_v1>() as u32,
            abi_version:1,generation:if state==0{0}else{generation},state,reserved:0})};
        Ok(0)
    })
}
unsafe extern "C" fn originate<F:Frontend>(ctx:*mut c_void,port:*const u8,account:*const u8,generation:u64,
    destination:abi::rptadv_peer_bytes_v1,maximum_frames:usize,current:abi::rptadv_current_v1,current_context:*mut c_void,
    out:*mut *mut c_void)->i32 {
    boundary(||{
        let out=unsafe{out.as_mut()}.ok_or(Error::Input)?;
        *out=ptr::null_mut();
        if maximum_frames==0 || generation==0 {return Err(Error::Input);}
        let current=current.ok_or(Error::Input)?;
        let host=unsafe{F::host(ctx)}.ok_or(Error::Input)?;
        let port=unsafe{port.cast::<[u8;16]>().as_ref()}.ok_or(Error::Input)?;
        let account=unsafe{account.cast::<[u8;16]>().as_ref()}.ok_or(Error::Input)?;
        let target=std::str::from_utf8(unsafe{bytes(destination,64)}?).map_err(|_|Error::Input)?;
        let call=host.originate(*port,*account,generation,target,||unsafe{current(current_context)!=0})?;
        if unsafe{current(current_context)}==0{return Err(Error::Retired);}
        let handle=F::wrap(call);
        if handle.is_null(){return Err(Error::Provider);}
        *out=handle;Ok(0)
    })
}
unsafe extern "C" fn send<F:Frontend>(_: *mut c_void,peer:*mut c_void,data:*const u8,count:usize,
    frames:u32,timestamp:u32,deadline:u32)->i32{
    boundary(||{
        if count==0 || count>65527 || data.is_null() || frames==0 {return Err(Error::Input);}
        let call=unsafe{F::call(peer)}.ok_or(Error::Input)?;
        call.send(unsafe{slice::from_raw_parts(data,count)},frames,timestamp,deadline)
    })
}
unsafe extern "C" fn now_ns()->u64{
    let mut value=std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe{libc::clock_gettime(libc::CLOCK_MONOTONIC,value.as_mut_ptr())}!=0{return 0;}
    let value=unsafe{value.assume_init()};
    if !(0..1_000_000_000).contains(&value.tv_nsec){return 0;}
    u64::try_from(value.tv_sec).ok().and_then(|s|s.checked_mul(1_000_000_000))
        .and_then(|s|s.checked_add(value.tv_nsec as u64)).unwrap_or(0)
}
unsafe extern "C" fn clock(_: *mut c_void,out:*mut abi::rptadv_media_clock_reader_v1)->i32{
    boundary(||{
        if out.is_null() || unsafe{ptr::addr_of!((*out).struct_size).read_unaligned()}<(size_of::<abi::rptadv_media_clock_reader_v1>()as u32)
            || unsafe{ptr::addr_of!((*out).abi_version).read_unaligned()}!=1 || unsafe{now_ns()}==0 {return Err(Error::Input);}
        unsafe{out.write(abi::rptadv_media_clock_reader_v1{
            struct_size:size_of::<abi::rptadv_media_clock_reader_v1>() as u32,abi_version:1,clock_kind:1,reserved:0,read_ns:Some(now_ns)})};
        Ok(0)
    })
}

/// Complete native operations for one concrete standalone or Asterisk host.
pub const fn operations<F:Frontend>()->abi::rptadv_phone_provider_v1{
    abi::rptadv_phone_provider_v1 {
        struct_size:size_of::<abi::rptadv_phone_provider_v1>() as u32,abi_version:1,
        prepare:Some(prepare::<F>),apply:Some(drive::<F,false>),restore:Some(drive::<F,true>),
        release:Some(release::<F>),observe:Some(observe::<F>),originate:Some(originate::<F>),
        send_prepared_timed:Some(send::<F>),media_clock_reader:Some(clock),
    }
}

/// Submit one genuine native incoming dialog. Authorization borrows the peer.
/// The product's incoming operation consumes the unique handle on success or
/// terminal negative return; a positive result leaves it with the frontend.
pub fn incoming<F:Frontend>(call:PrivateCall,
    authorize:impl FnOnce(*const abi::rptadv_incoming_peer_v1,*mut c_void)->i32,
    incoming:impl FnOnce(*const abi::rptadv_incoming_peer_v1,*mut c_void)->i32) {
    let answer=call.answer_permit();
    let (local,_,remote)=call.identity();
    let source=call.source().ip().to_string();
    let span=|s:&str| abi::rptadv_peer_bytes_v1{data:s.as_ptr(),length:s.len()};
    let request=abi::rptadv_incoming_peer_v1{
        struct_size:size_of::<abi::rptadv_incoming_peer_v1>() as u32,abi_version:1,protocol:3,
        local:span(local),remote:span(remote),source:span(&source),
    };
    let raw=F::wrap(call);
    if raw.is_null(){return;}
    if authorize(&request,raw)!=0 {unsafe{F::discard(raw)};return;}
    match incoming(&request,raw) {
        0=>{let _=answer.answer();}
        n if n>0=>unsafe{F::discard(raw)},
        _=>{}
    }
}

/// Copy authenticated native peer facts only while the original dialog lives.
///
/// # Safety
/// Output has a readable initial size and writable storage through that size.
/// All referenced bytes live with the call, as required by the product peer ABI.
pub unsafe fn info(call:&PrivateCall,out:*mut abi::rptadv_peer_info_v1)->i32{
    boundary(|| {
        if out.is_null()||unsafe{ptr::addr_of!((*out).struct_size).read_unaligned()}<(size_of::<abi::rptadv_peer_info_v1>() as u32)
            ||!call.live(){return Err(Error::Retired);}
        let (local,account,remote)=call.identity();
        let (account_generation,qso_generation)=call.generations();
        let span=|s:&str|abi::rptadv_peer_bytes_v1{data:s.as_ptr(),length:s.len()};
        unsafe{out.write(abi::rptadv_peer_info_v1{
            struct_size:size_of::<abi::rptadv_peer_info_v1>() as u32,abi_version:1,protocol:3,capabilities:51 | call.digit_capabilities(),
            account_generation,qso_generation,local:span(local),remote:span(remote),
            callsign:abi::rptadv_peer_bytes_v1{data:ptr::null(),length:0},provider_account:span(account),
        })};
        Ok(0)
    })
}

/// # Safety
/// PCM, output and result are caller-owned ABI buffers with their advertised capacities.
pub unsafe fn prepare_audio(call:&PrivateCall, pcm:*const f32, count:usize, bytes:*mut u8,
    capacity:usize, flush:u32, result:*mut abi::rptadv_codec_encode_result_v1)->i32{
    boundary(||{
        if result.is_null()||unsafe{ptr::addr_of!((*result).struct_size).read_unaligned()}<(size_of::<abi::rptadv_codec_encode_result_v1>()as u32)
            ||count>isize::MAX as usize/size_of::<f32>() ||capacity>isize::MAX as usize
            ||(count!=0&&pcm.is_null())||(capacity!=0&&bytes.is_null()){return Err(Error::Input);}
        let pcm=if count==0{&[]}else{unsafe{slice::from_raw_parts(pcm,count)}};
        let bytes=if capacity==0{&mut[]}else{unsafe{slice::from_raw_parts_mut(bytes,capacity)}};
        call.prepare(pcm,bytes,flush,unsafe{&mut *result})
    })
}

/// # Safety
/// The sink and output follow the checked peer-source ABI for this synchronous read.
pub unsafe fn read(call:&PrivateCall, completion:u32, sink:*const abi::rptadv_peer_source_sink_v1,
    output:*mut abi::rptadv_peer_source_result_v1)->i32{
    boundary(||{
        if output.is_null()||unsafe{ptr::addr_of!((*output).struct_size).read_unaligned()}<(size_of::<abi::rptadv_peer_source_result_v1>()as u32)
            ||unsafe{ptr::addr_of!((*output).abi_version).read_unaligned()}!=1{return Err(Error::Input);}
        unsafe{call.read(completion,sink,&mut *output)}?;
        Ok(0)
    })
}
