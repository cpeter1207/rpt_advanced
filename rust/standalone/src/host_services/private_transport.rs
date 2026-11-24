//! Concrete host context and unique peer ownership for the shared private transports.
use super::*;
use rptadv_private_phone_host::{calls::PrivateCall, frontend::{self, Frontend},
    selection::{NativePaths, PhoneHost}};
struct Adapter;
unsafe impl Frontend for Adapter {
    unsafe fn host<'a>(ctx:*mut c_void)->Option<&'a PhoneHost> {
        unsafe{ctx.cast::<StandaloneHostContext>().as_ref()}?.phone.as_ref()
    }
    unsafe fn call<'a>(peer:*mut c_void)->Option<&'a PrivateCall> {
        if let HostPeer::Phone(call)=unsafe{peer.cast::<PeerHandle>().as_ref()}? {Some(call)} else {None}
    }
    fn wrap(call:PrivateCall)->*mut c_void {Box::into_raw(Box::new(PeerHandle::Phone(call))).cast()}
    unsafe fn discard(peer:*mut c_void){unsafe{drop(Box::from_raw(peer.cast::<PeerHandle>()))};}
}
static OPERATIONS:rptadv_private_phone_host::abi::rptadv_phone_provider_v1=frontend::operations::<Adapter>();
impl HostServicesOwner {
    pub(crate) fn install_private(&mut self, product:&abi::rptadv_product_descriptor_v1) {
        let (Some(authorize),Some(incoming))=(product.authorize_incoming,product.incoming) else{return;};
        let directory=std::path::Path::new(env!("RPT_ADVANCED_LIBDIR"));
        let paths=NativePaths{sip:directory.join("librptadv_sip_adapter.so.1"),jitter:directory.join("librptadv_jitter_adapter.so.1"),iax:"librptadviax2.so.2".into(),g711:"librptadv_codec_g711.so.1".into()};
        let deliver=Arc::new(move|call|frontend::incoming::<Adapter>(call,
            |req,raw|unsafe{authorize(req.cast(),raw)},|req,raw|unsafe{incoming(req.cast(),raw)}));
        self.context.phone=Some(PhoneHost::new(paths,deliver));
        self.descriptor.phone_provider=std::ptr::from_ref(&OPERATIONS).cast();
    }
    pub(crate) fn private_stopped(&self)->bool{
        self.context.phone.as_ref().is_none_or(|host|host.stop()==Ok(true))
    }
}
