use crate::{TransportError, TransportErrorKind, TransportPolicy};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

pub(crate) const MAXIMUM_DNS_ANSWERS: usize = 64;

pub(crate) fn parse_url(input: &str) -> Result<reqwest::Url, TransportError> {
    if input.len() > 8 * 1024 || input.chars().any(char::is_control) {
        return Err(TransportError::input(
            "URL exceeds the bound or contains control characters",
        ));
    }
    let url = reqwest::Url::parse(input)
        .map_err(|_| TransportError::input("invalid absolute HTTP URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port_or_known_default().is_none_or(|port| port == 0)
    {
        return Err(TransportError::input(
            "HTTP(S), an explicit host, and no URL credentials or fragment are required",
        ));
    }
    Ok(url)
}

pub(crate) fn literal_address(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

pub(crate) fn validate_addresses(
    url: &reqwest::Url,
    addresses: &[SocketAddr],
    policy: &TransportPolicy,
) -> Result<SocketAddr, TransportError> {
    if addresses.is_empty() || addresses.len() > MAXIMUM_DNS_ANSWERS {
        return Err(TransportError::new(
            TransportErrorKind::AddressPolicy,
            "DNS answers are absent or exceed the admission bound",
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| TransportError::input("missing port"))?;
    let literal = url.host_str().and_then(literal_address);
    for address in addresses {
        if address.port() != port || literal.is_some_and(|ip| ip != address.ip()) {
            return Err(TransportError::new(
                TransportErrorKind::AddressPolicy,
                "address does not match the requested host and port",
            ));
        }
        let permitted_loopback =
            address.ip().is_loopback() && policy.allow_loopback == Some(*address);
        if !permitted_loopback && !public_address(address.ip()) {
            return Err(TransportError::new(
                TransportErrorKind::AddressPolicy,
                "destination is not an admitted public address or exact loopback permit",
            ));
        }
    }
    Ok(addresses[0])
}

fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return public_v4(mapped);
            }
            let segments = address.segments();
            // Reuse the prerequisite special-use exclusions, additionally require global unicast.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && segments[0] & 0xfff0 != 0x3ff0
        }
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 192 && b == 31 && c == 196)
        || (a == 192 && b == 52 && c == 193)
        || (a == 192 && b == 175 && c == 48)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}
