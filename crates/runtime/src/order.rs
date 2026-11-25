//! Start order: a plugin starts after the plugins whose interfaces it
//! imports. Plugins in a cycle, or that depend on one that is missing or
//! in a cycle, are refused.

use std::collections::{BTreeMap, BTreeSet};

/// Returns the start order and the refused plugins with the reason.
/// `deps` maps each plugin to the plugins it imports from.
pub fn start_order(
    deps: &BTreeMap<String, Vec<String>>,
) -> (Vec<String>, BTreeMap<String, String>) {
    let mut order = Vec::new();
    let mut refused = BTreeMap::new();
    let mut done = BTreeSet::new();
    // Kahn's algorithm over the plugins whose dependencies all exist.
    let mut pending: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, ds) in deps {
        if let Some(missing) = ds.iter().find(|d| !deps.contains_key(*d)) {
            refused.insert(
                name.clone(),
                format!("depends on {missing}, which is not installed"),
            );
        } else {
            pending.insert(name, ds.iter().map(String::as_str).collect());
        }
    }
    loop {
        let ready: Vec<&str> = pending
            .iter()
            .filter(|(_, ds)| ds.iter().all(|d| done.contains(*d)))
            .map(|(n, _)| *n)
            .collect();
        if ready.is_empty() {
            break;
        }
        for name in ready {
            pending.remove(name);
            done.insert(name);
            order.push(name.to_string());
        }
    }
    // Whatever is left waits on a cycle or on a plugin refused above.
    let missing = refused.clone();
    for (name, ds) in pending {
        let why = match ds.iter().find(|d| missing.contains_key(**d)) {
            Some(d) => format!("depends on {d}, which was refused"),
            None => "is part of an import cycle".to_string(),
        };
        refused.insert(name.to_string(), why);
    }
    (order, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deps(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(n, ds)| (n.to_string(), ds.iter().map(|d| d.to_string()).collect()))
            .collect()
    }

    #[test]
    fn dependencies_start_first() {
        let (order, refused) = start_order(&deps(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]));
        assert_eq!(order, ["a", "b", "c"]);
        assert!(refused.is_empty());
    }

    #[test]
    fn cycles_and_missing_dependencies_are_refused() {
        let (order, refused) = start_order(&deps(&[
            ("a", &[]),
            ("x", &["y"]),
            ("y", &["x"]),
            ("m", &["gone"]),
            ("n", &["m"]),
        ]));
        assert_eq!(order, ["a"]);
        assert!(refused["x"].contains("cycle"));
        assert!(refused["y"].contains("cycle"));
        assert!(refused["m"].contains("not installed"));
        assert!(refused["n"].contains("refused"));
    }
}
