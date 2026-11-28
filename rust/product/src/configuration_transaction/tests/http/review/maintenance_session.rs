//! Run the actual CLI against the real TLS coordinator and serialized synthetic radio.
use super::*;
use crate::abi;
use std::ffi::c_void;

pub(super) unsafe extern "C" fn control(
    context: *mut c_void,
    _: *mut c_void,
    req: *mut abi::rptadv_radio_control_v1,
    _: abi::rptadv_text_sink_v1,
    _: *mut c_void,
) -> i32 {
    // SAFETY: the fixture table and exclusive scalar request outlive this callback.
    let (state, req) = unsafe { (&*context.cast::<Counters>(), &mut *req) };
    if req.operation == 7 {
        req.command.value = 0;
        req.command.flags = 0;
        return 0;
    }
    let event = match req.operation {
        2 => {
            let before = req.command.value;
            match req.command.command {
                abi::URP_AST_COMMAND_GET_MIXER => req.command.value = 317,
                abi::URP_AST_COMMAND_READ_EEPROM => {
                    req.command.eeprom_words[63] = 925;
                }
                abi::URP_AST_COMMAND_SET_TEST_TONE
                | abi::URP_AST_COMMAND_SET_CTCSS_INHIBIT
                | abi::URP_AST_COMMAND_SET_SUBAUDIBLE_OVERRIDE => (),
                _ => return -7,
            };
            (req.command.command, before)
        }
        3 => (100, i64::from(req.keyed)),
        4 => (101, 0),
        _ => return -7,
    };
    state.maintenance.lock().unwrap().push(event);
    0
}
pub(super) fn exercise(client: &Client<'_>, resource: &str, counters: &Counters) {
    let cli = std::env::var_os("RPTADV_TEST_OPERATOR_CLI").expect("compiled operator CLI required");
    let result = Command::new("python3")
        .args(["-c", include_str!("maintenance_session.py")])
        .arg(cli)
        .arg(client.pki)
        .arg(client.port.to_string())
        .arg(resource)
        .status()
        .unwrap();
    assert!(
        result.success(),
        "actual operator CLI and connected lease requests"
    );
    wait_until(|| {
        counters
            .maintenance
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == 101)
            .count()
            >= 5
    });
    assert_eq!(
        *counters.maintenance.lock().unwrap(),
        [
            (abi::URP_AST_COMMAND_GET_MIXER, 0),
            (abi::URP_AST_COMMAND_READ_EEPROM, 0),
            (abi::URP_AST_COMMAND_SET_TEST_TONE, 1),
            (abi::URP_AST_COMMAND_SET_CTCSS_INHIBIT, 1),
            (abi::URP_AST_COMMAND_SET_SUBAUDIBLE_OVERRIDE, 1),
            (100, 1),
            (101, 0),
            (100, 1),
            (101, 0),
            (100, 1),
            (101, 0),
            (100, 1),
            (101, 0),
            (100, 1),
            (101, 0),
            (102, 0),
        ],
        "actual scalar callback sequence, exactly once per admitted operation and clear"
    );
}
