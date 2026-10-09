use std::net::UdpSocket;

/// CLI children must bind a preconfigured address after the reservation drops.
/// On Linux, bind(0) puts that address in the same ephemeral pool as concurrent
/// QUIC client endpoints, which can steal it before the child starts.
pub fn reserve_node_listen_socket() -> UdpSocket {
    #[cfg(target_os = "linux")]
    {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT_PORT: AtomicU32 = AtomicU32::new(0);
        let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
            .expect("read Linux ephemeral UDP port range");
        let mut values = range
            .split_whitespace()
            .map(|value| value.parse::<u16>().unwrap());
        let first = values.next().unwrap();
        let last = values.next().unwrap();
        for _ in 10000..=u16::MAX {
            let offset = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
            let port = 10000 + ((std::process::id().wrapping_add(offset)) % 55536) as u16;
            if (first..=last).contains(&port) {
                continue;
            }
            match UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port)) {
                Ok(socket) => return socket,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(error) => panic!("reserve CLI node port {port}: {error}"),
            }
        }
        panic!("no available CLI node port outside the Linux ephemeral range");
    }
    #[cfg(not(target_os = "linux"))]
    UdpSocket::bind("127.0.0.1:0").unwrap()
}
