//! This utility module contains some network interface conveniences.

use if_addrs::Interface;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;

use super::errors::KaboodleError;

/// Creates a pair of sockets for receiving and sending multicast messages on the given port with
/// the given  interface.
pub fn create_broadcast_sockets(
    interface: &Interface,
    broadcast_port: &u16,
) -> Result<(UdpSocket, UdpSocket, SocketAddr), KaboodleError> {
    match interface.ip() {
        IpAddr::V4(_) => {
            // IPv4:
            // - for listening to broadcasts, we need to bind our socket to 0.0.0.0:<port>
            // - for sending broadcasts, we need to send to 255.255.255.255:<port>.
            let broadcast_inbound_addr = SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(0, 0, 0, 0),
                *broadcast_port,
            ));
            let broadcast_outbound_addr = SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(255, 255, 255, 255),
                *broadcast_port,
            ));
            let broadcast_raw_sock: std::net::UdpSocket = {
                let broadcast_sock =
                    Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
                broadcast_sock.set_broadcast(true).unwrap();
                broadcast_sock.set_nonblocking(true).unwrap();
                broadcast_sock.set_reuse_address(true).unwrap();
                broadcast_sock.set_reuse_port(true).unwrap();
                broadcast_sock
                    .bind(&SockAddr::from(broadcast_inbound_addr))
                    .expect("Failed to bind for broadcast");
                broadcast_sock.into()
            };

            // Unlike IPv6, we can use a single socket for both sending and receiving broadcasts,
            // but consistency for consuming code, it makes sense to return two here as well.
            let broadcast_in_sock = UdpSocket::from_std(broadcast_raw_sock.try_clone()?)?;
            let broadcast_out_sock = UdpSocket::from_std(broadcast_raw_sock)?;

            Ok((
                broadcast_in_sock,
                broadcast_out_sock,
                broadcast_outbound_addr,
            ))
        }
        IpAddr::V6(_) => {
            // IPv6:
            // - for listening to broadcasts, we have to join a multicast IP address.
            // - for sending broadcast messages, we have to explicitly tell the socket which
            //   network interface to use, otherwise it won't have a route to the multicast IP.

            let Some(interface_idx) = interface.index else {
                return Err(KaboodleError::UnableToFindInterfaceNumber);
            };

            // All IPv6 IP addresses in the ff00::/8 prefix are multicast addresses. The first 16
            // bits of the IP indicate the "multicast group", and the remaining 112 bits are the
            // "group ID". A prefix of 0xff02 indicates link-local multicast, where "link-local"
            // means the traffic won't pass beyond the local router.
            // Broadly speaking, we can use whatever group ID we like, so long as it doesn't
            // conflict with any well-known services.
            // See https://www.ciscopress.com/articles/article.asp?p=2803866&seqNum=5 for more
            // information about IPv6 multicast addresses.
            let broadcast_ip_addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0x1213, 0x1989);

            let broadcast_socket_addr =
                SocketAddr::new(IpAddr::V6(broadcast_ip_addr), *broadcast_port);
            let broadcast_in_sock = {
                let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
                sock.join_multicast_v6(&broadcast_ip_addr, interface_idx)?;
                sock.set_nonblocking(true)?;
                sock.set_only_v6(true)?;
                sock.set_reuse_address(true)?;
                sock.set_reuse_port(true)?;
                sock.bind(&SockAddr::from(SocketAddr::new(
                    Ipv6Addr::UNSPECIFIED.into(),
                    *broadcast_port,
                )))?;

                UdpSocket::from_std(sock.into())?
            };
            let broadcast_out_sock = {
                let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
                sock.set_multicast_if_v6(interface_idx)?;
                sock.set_nonblocking(true)?;
                sock.set_reuse_address(true)?;
                sock.set_reuse_port(true)?;
                sock.bind(&SockAddr::from(SocketAddr::new(
                    Ipv6Addr::UNSPECIFIED.into(),
                    0,
                )))?;

                UdpSocket::from_std(sock.into())?
            };

            Ok((broadcast_in_sock, broadcast_out_sock, broadcast_socket_addr))
        }
    }
}

/// Get all non-loopback interfaces for this host.
pub fn non_loopback_interfaces() -> Vec<Interface> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|addr| !addr.is_loopback())
        .collect()
}
