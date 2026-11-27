use crate::Level;

const RESERVED: [&str; 6] = ["core", "sql", "http", "llm", "agent", "ctl"];

/// Whether `kind` matches `^(core|sql|http|llm|agent|ctl|x-[a-z0-9-]+)\.[a-z0-9_.]+$`.
pub fn is_valid_kind(kind: &str) -> bool {
    let Some((ns, rest)) = kind.split_once('.') else {
        return false;
    };
    let ns_ok = RESERVED.contains(&ns)
        || ns
            .strip_prefix("x-")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
    let rest_ok = !rest.is_empty()
        && rest
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
    ns_ok && rest_ok
}

/// The level an event gets when `meta.level` is absent.
pub fn default_level(kind: &str) -> Level {
    match kind {
        "core.exception" => Level::Error,
        _ => Level::Debug,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_reserved_and_vendor_namespaces() {
        for k in ["core.value", "sql.query", "llm.call", "agent.tool_call", "x-laravel.job", "x-a1-b.c.d"] {
            assert!(is_valid_kind(k), "{k}");
        }
    }

    #[test]
    fn rejects_malformed_kinds() {
        for k in ["", "core", "core.", "Core.value", "foo.bar", "x-.bar", "x-Foo.bar", "core.Value", "core.a b"] {
            assert!(!is_valid_kind(k), "{k}");
        }
    }
}
