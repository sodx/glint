//! Expectation matching for `spec/conformance/scenarios.json` (README "How expectations match").

use serde_json::Value;

/// Appends one message per mismatch, each prefixed with the JSON path.
pub fn check(expected: &Value, actual: &Value, path: &str, errs: &mut Vec<String>) {
    match expected {
        Value::String(s) if s.starts_with('$') => placeholder(s, actual, path, errs),
        Value::Object(exp) => {
            let Some(act) = actual.as_object() else {
                errs.push(format!("{path}: expected an object, got {}", short(actual)));
                return;
            };
            for (k, v) in exp {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                match act.get(k) {
                    Some(a) => check(v, a, &p, errs),
                    None => errs.push(format!("{p}: missing")),
                }
            }
        }
        Value::Array(exp) => {
            let Some(act) = actual.as_array() else {
                errs.push(format!("{path}: expected an array, got {}", short(actual)));
                return;
            };
            if exp.len() != act.len() {
                errs.push(format!("{path}: expected {} items, got {}", exp.len(), act.len()));
                return;
            }
            for (i, (e, a)) in exp.iter().zip(act).enumerate() {
                check(e, a, &format!("{path}[{i}]"), errs);
            }
        }
        Value::Number(e) => {
            let same = matches!((e.as_f64(), actual.as_f64()), (Some(x), Some(y)) if x == y);
            if !same {
                errs.push(format!("{path}: expected {e}, got {}", short(actual)));
            }
        }
        _ => {
            if expected != actual {
                errs.push(format!("{path}: expected {}, got {}", short(expected), short(actual)));
            }
        }
    }
}

fn placeholder(p: &str, actual: &Value, path: &str, errs: &mut Vec<String>) {
    let ok = match p {
        "$any" => true,
        "$string" => actual.as_str().is_some_and(|s| !s.is_empty()),
        "$int" => actual.as_i64().is_some() || actual.as_u64().is_some(),
        "$number" => actual.is_number(),
        "$bool" => actual.is_boolean(),
        "$nonempty-array" => actual.as_array().is_some_and(|a| !a.is_empty()),
        _ => {
            if let Some(n) = p.strip_prefix("$gte:") {
                let n: f64 = n.parse().unwrap_or(f64::INFINITY);
                actual.as_f64().is_some_and(|a| a >= n)
            } else if let Some(t) = p.strip_prefix("$contains:") {
                actual.as_str().is_some_and(|s| s.contains(t))
            } else if let Some(k) = p.strip_prefix("$has-key-deep:") {
                has_key_deep(actual, k)
            } else {
                errs.push(format!("{path}: unknown placeholder {p} in scenarios.json"));
                return;
            }
        }
    };
    if !ok {
        let what = match p {
            "$string" => "a non-empty string".to_owned(),
            "$int" => "an integer".to_owned(),
            "$number" => "a number".to_owned(),
            "$bool" => "a boolean".to_owned(),
            "$nonempty-array" => "a non-empty array".to_owned(),
            _ if p.starts_with("$gte:") => format!("a number >= {}", &p[5..]),
            _ if p.starts_with("$contains:") => format!("a string containing {:?}", &p[10..]),
            _ if p.starts_with("$has-key-deep:") => format!("the key {:?} somewhere inside", &p[14..]),
            _ => p.to_owned(),
        };
        errs.push(format!("{path}: expected {what}, got {}", short(actual)));
    }
}

fn has_key_deep(v: &Value, key: &str) -> bool {
    match v {
        Value::Object(m) => m.contains_key(key) || m.values().any(|x| has_key_deep(x, key)),
        Value::Array(a) => a.iter().any(|x| has_key_deep(x, key)),
        _ => false,
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 { format!("{}…", s.chars().take(120).collect::<String>()) } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn errs(e: Value, a: Value) -> Vec<String> {
        let mut v = Vec::new();
        check(&e, &a, "", &mut v);
        v
    }

    #[test]
    fn subset_objects_and_exact_arrays() {
        assert!(errs(json!({"a": 1}), json!({"a": 1, "b": 2})).is_empty());
        assert!(errs(json!({"a": 1.0}), json!({"a": 1})).is_empty());
        assert_eq!(errs(json!({"a": 1}), json!({"b": 1})), ["a: missing"]);
        assert_eq!(errs(json!([1, 2]), json!([1])), [": expected 2 items, got 1"]);
        assert_eq!(errs(json!({"x": [{"y": "z"}]}), json!({"x": [{"y": "w"}]})), ["x[0].y: expected \"z\", got \"w\""]);
    }

    #[test]
    fn placeholders() {
        assert!(errs(json!("$has-key-deep:$ref"), json!({"a": {"self": {"$ref": "$"}}})).is_empty());
        assert!(!errs(json!("$has-key-deep:$ref"), json!({"a": 1})).is_empty());
        assert!(errs(json!("$gte:15"), json!(20.5)).is_empty());
        assert!(!errs(json!("$gte:15"), json!(3)).is_empty());
        assert!(errs(json!("$contains:Order"), json!("App\\ConformanceOrder")).is_empty());
        assert!(!errs(json!("$string"), json!("")).is_empty());
        assert!(errs(json!("$int"), json!(3)).is_empty());
        assert!(!errs(json!("$nonempty-array"), json!([])).is_empty());
    }
}
