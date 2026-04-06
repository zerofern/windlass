use crate::types::*;
use windlass_types::{AuthCookie, VpnIp, VpnPort};
use std::net::Ipv4Addr;

pub fn ip() -> VpnIp {
    VpnIp(Ipv4Addr::new(10, 8, 0, 1))
}

pub fn port() -> VpnPort {
    VpnPort::try_new(51820).unwrap()
}

pub fn cookie() -> AuthCookie {
    AuthCookie("sid=abc".into())
}

pub fn connected_state() -> SystemState {
    SystemState {
        vpn: VpnState::Connected {
            ip: ip(),
            port: port(),
        },
        qbit: QbitState::Ready { port: port(), cookie: cookie() },
        mam: MamState::Synced {
            port: port(),
            ip: ip(),
        },
        ..SystemState::initial()
    }
}
