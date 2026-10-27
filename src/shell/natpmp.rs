//! Request TCP and UDP mappings from a NAT-PMP gateway (RFC 6886), as Proton VPN
//! port forwarding requires.

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use tokio::net::UdpSocket;

use crate::core::{MappingFailure, NatPmpRefusal, Port, PortForwardingEvent};

const UDP: u8 = 1;
const TCP: u8 = 2;
/// RFC 6886 retransmission delays, shortened: a renewal is retried by the core.
const ATTEMPT_TIMEOUTS: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// A mapping request. Like Proton VPN's documented `natpmpc -a 1 0 <proto> 60`,
/// the internal port is 0 and the suggested public port is 1, so the gateway
/// chooses the port.
fn request(opcode: u8, lifetime: Duration) -> [u8; 12] {
    let lifetime = u32::try_from(lifetime.as_secs())
        .unwrap_or(u32::MAX)
        .to_be_bytes();
    let [l0, l1, l2, l3] = lifetime;
    [0, opcode, 0, 0, 0, 0, 0, 1, l0, l1, l2, l3]
}

/// The granted public port and lifetime, or why there is none.
fn response(opcode: u8, packet: &[u8]) -> Result<(Port, Duration), MappingFailure> {
    let &[
        version,
        op,
        r0,
        r1,
        _,
        _,
        _,
        _,
        _,
        _,
        p0,
        p1,
        l0,
        l1,
        l2,
        l3,
    ] = packet
    else {
        return Err(MappingFailure::Malformed);
    };
    if version != 0 || op != opcode + 128 {
        return Err(MappingFailure::Malformed);
    }
    let reason = match u16::from_be_bytes([r0, r1]) {
        0 => None,
        1 => Some(NatPmpRefusal::UnsupportedVersion),
        2 => Some(NatPmpRefusal::NotAuthorized),
        3 => Some(NatPmpRefusal::NetworkFailure),
        4 => Some(NatPmpRefusal::OutOfResources),
        5 => Some(NatPmpRefusal::UnsupportedOpcode),
        _ => return Err(MappingFailure::Malformed),
    };
    if let Some(reason) = reason {
        return Err(MappingFailure::Refused { reason });
    }
    let port = Port::new(u16::from_be_bytes([p0, p1])).ok_or(MappingFailure::Malformed)?;
    let lifetime = Duration::from_secs(u64::from(u32::from_be_bytes([l0, l1, l2, l3])));
    Ok((port, lifetime))
}

async fn map(
    socket: &UdpSocket,
    gateway: SocketAddrV4,
    opcode: u8,
    lifetime: Duration,
) -> Result<(Port, Duration), MappingFailure> {
    let packet = request(opcode, lifetime);
    let mut buffer = [0_u8; 64];
    for timeout in ATTEMPT_TIMEOUTS {
        socket
            .send_to(&packet, gateway)
            .await
            .map_err(|_| MappingFailure::NoResponse)?;
        // Answers to earlier attempts may arrive late; any valid one will do.
        if let Ok(Ok((length, from))) =
            tokio::time::timeout(timeout, socket.recv_from(&mut buffer)).await
            && from == gateway.into()
        {
            return response(opcode, buffer.get(..length).unwrap_or_default());
        }
    }
    Err(MappingFailure::NoResponse)
}

/// Map UDP and TCP ports for `lifetime` and report the result as a core event.
/// Without a configured gateway, nothing answers.
pub(super) async fn map_ports(
    gateway: Option<SocketAddrV4>,
    lifetime: Duration,
) -> PortForwardingEvent {
    let mapped = async {
        let gateway = gateway.ok_or(MappingFailure::NoResponse)?;
        let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|_| MappingFailure::NoResponse)?;
        let (udp, udp_lifetime) = map(&socket, gateway, UDP, lifetime).await?;
        let (tcp, tcp_lifetime) = map(&socket, gateway, TCP, lifetime).await?;
        Ok(PortForwardingEvent::Mapped {
            tcp,
            udp,
            lifetime: udp_lifetime.min(tcp_lifetime),
        })
    };
    mapped
        .await
        .unwrap_or_else(|reason| PortForwardingEvent::MappingFailed { reason })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::core::{MappingFailure, NatPmpRefusal, Port};

    use crate::core::PortForwardingEvent;

    use super::{TCP, UDP, map_ports, request, response};

    #[tokio::test]
    async fn without_a_gateway_nothing_answers() {
        assert_eq!(
            map_ports(None, Duration::from_mins(1)).await,
            PortForwardingEvent::MappingFailed {
                reason: MappingFailure::NoResponse
            }
        );
    }

    #[test]
    fn requests_let_the_gateway_choose_the_port() {
        assert_eq!(
            request(TCP, Duration::from_mins(1)),
            [0, 2, 0, 0, 0, 0, 0, 1, 0, 0, 0, 60]
        );
    }

    #[test]
    fn responses_are_decoded_and_checked() -> Result<(), &'static str> {
        let granted = [0, 129, 0, 0, 0, 0, 0, 9, 0, 0, 0xa0, 0x28, 0, 0, 0, 60];
        let port = Port::new(41000).ok_or("zero port")?;
        assert_eq!(response(UDP, &granted), Ok((port, Duration::from_mins(1))));
        for (code, refusal) in [
            (1, NatPmpRefusal::UnsupportedVersion),
            (2, NatPmpRefusal::NotAuthorized),
            (3, NatPmpRefusal::NetworkFailure),
            (4, NatPmpRefusal::OutOfResources),
            (5, NatPmpRefusal::UnsupportedOpcode),
        ] {
            let refused = [0, 130, 0, code, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0];
            assert_eq!(
                response(TCP, &refused),
                Err(MappingFailure::Refused { reason: refusal }),
                "result code {code}"
            );
        }
        let unknown = [0, 130, 0, 6, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(response(TCP, &unknown), Err(MappingFailure::Malformed));
        assert_eq!(
            response(TCP, &granted),
            Err(MappingFailure::Malformed),
            "an answer for another protocol"
        );
        assert_eq!(
            response(UDP, &granted[..12]),
            Err(MappingFailure::Malformed)
        );
        let mut zero_port = granted;
        zero_port[10] = 0;
        zero_port[11] = 0;
        assert_eq!(response(UDP, &zero_port), Err(MappingFailure::Malformed));
        let mut wrong_version = granted;
        wrong_version[0] = 1;
        assert_eq!(
            response(UDP, &wrong_version),
            Err(MappingFailure::Malformed)
        );
        Ok(())
    }
}
