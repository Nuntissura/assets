//! Host-executed wall queries and bounded, framework-independent interaction state.
//!
//! A host snapshot must freeze membership AND sort keys across requests. These
//! reducers validate pages; they neither execute a database query nor grant access.
mod model;
mod paging;
mod wall;

pub use model::*;
pub use paging::*;
pub use wall::*;

fn invalid(field: &str) -> rss_contracts::Error {
    rss_contracts::Error::Invalid {
        field: field.into(),
    }
}

fn text(value: &str, field: &str, maximum: usize) -> rss_contracts::Result<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(invalid(field));
    }
    if value.len() > maximum {
        return Err(rss_contracts::Error::LimitExceeded {
            field: field.into(),
        });
    }
    Ok(())
}

fn host_ref(
    value: &rss_contracts::HostRef,
    bounds: &rss_contracts::Bounds,
) -> rss_contracts::Result<()> {
    for part in [&value.host_instance_id, &value.owner_kind, &value.native_id] {
        text(part, "host_ref", bounds.max_text_bytes)?;
    }
    Ok(())
}

fn encoded_bound<T: serde::Serialize>(
    value: &T,
    bounds: &rss_contracts::Bounds,
) -> rss_contracts::Result<()> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let next = self
                .bytes
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("encoded boundary exceeds limit"))?;
            self.bytes = next;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(
        Counter {
            bytes: 0,
            limit: bounds.max_operation_bytes,
        },
        value,
    )
    .map_err(|_| rss_contracts::Error::LimitExceeded {
        field: "query.encoded_bytes".into(),
    })
}
