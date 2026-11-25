use std::fmt;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;
use std::str::FromStr;

pub const DEFAULT_LAN_PORT: u16 = 53980;
const DEVICE_ID_DIGITS: usize = 9;

/// A device's routing identifier within a signaling service.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DeviceId(String);

impl FromStr for DeviceId {
    type Err = TargetError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let id = input.trim();
        if id.len() != DEVICE_ID_DIGITS || !id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(TargetError::DeviceId);
        }
        Ok(Self(id.to_owned()))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A literal unicast IP and a nonzero port for direct connections.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LanEndpoint {
    ip: IpAddr,
    port: NonZeroU16,
}

impl LanEndpoint {
    pub fn parse(address: &str, port: &str) -> Result<Self, TargetError> {
        let address = address.trim();
        let ip = if let Some(literal) = address
            .strip_prefix('[')
            .and_then(|literal| literal.strip_suffix(']'))
        {
            literal.parse::<Ipv6Addr>().map(IpAddr::V6)
        } else {
            address.parse::<IpAddr>()
        }
        .map_err(|_| TargetError::LanAddress)?
        .to_canonical();

        let unsupported = match ip {
            IpAddr::V4(ip) => ip.is_broadcast(),
            // A link-local target also needs a zone, which this interface
            // does not accept yet.
            IpAddr::V6(ip) => ip.is_unicast_link_local(),
        };
        if ip.is_unspecified() || ip.is_multicast() || unsupported {
            return Err(TargetError::LanAddress);
        }

        let port = port.trim();
        if !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(TargetError::LanPort);
        }
        let port = port
            .parse::<NonZeroU16>()
            .map_err(|_| TargetError::LanPort)?;

        Ok(Self { ip, port })
    }

    pub fn socket_addr(self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port.get())
    }
}

impl fmt::Display for LanEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.socket_addr().fmt(f)
    }
}

/// The desktop receiver binds only to a private interface or loopback.
pub fn local_listener_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetError {
    DeviceId,
    LanAddress,
    LanPort,
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DeviceId => "enter a nine-digit device ID",
            Self::LanAddress => "enter a supported IPv4 or IPv6 address",
            Self::LanPort => "enter a decimal port between 1 and 65535",
        })
    }
}

impl std::error::Error for TargetError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_preserve_leading_zeroes() -> Result<(), TargetError> {
        let id: DeviceId = " 012345678 ".parse()?;
        assert_eq!(id.to_string(), "012345678");
        for input in ["12345678", "1234567890", "123-45678", "１２３４５６７８９"] {
            assert_eq!(input.parse::<DeviceId>(), Err(TargetError::DeviceId));
        }
        Ok(())
    }

    #[test]
    fn endpoints_accept_literal_addresses_and_render_ipv6_with_brackets() -> Result<(), TargetError>
    {
        for (input, expected) in [
            ("192.168.1.20", "192.168.1.20:53980"),
            ("::ffff:192.168.1.20", "192.168.1.20:53980"),
            ("2001:db8::20", "[2001:db8::20]:53980"),
            ("[2001:db8::20]", "[2001:db8::20]:53980"),
        ] {
            assert_eq!(LanEndpoint::parse(input, "53980")?.to_string(), expected);
        }
        Ok(())
    }

    #[test]
    fn endpoints_reject_invalid_or_unusable_targets() {
        for input in [
            "localhost",
            "192.168.1.20:53980",
            "[192.168.1.20]",
            "0.0.0.0",
            "::",
            "255.255.255.255",
            "224.0.0.1",
            "ff02::1",
            "fe80::1",
            "fe80::1%3",
        ] {
            assert_eq!(
                LanEndpoint::parse(input, "53980"),
                Err(TargetError::LanAddress)
            );
        }
        for port in ["", "0", "65536", "+80", "0xffff", "-1"] {
            assert_eq!(
                LanEndpoint::parse("127.0.0.1", port),
                Err(TargetError::LanPort)
            );
        }
    }
}
