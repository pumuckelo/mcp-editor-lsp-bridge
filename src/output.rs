//! Agent-facing response shaping, shared by every inbound adapter.
use serde_json::{Value, json};
pub fn status(value: Value, verbose: bool) -> Value {
    if verbose {
        return value;
    }
    if let Some(workspaces) = value["workspaces"].as_array() {
        return json!({"workspaces":workspaces.iter().cloned().map(|w| status(w, false)).collect::<Vec<_>>(),"disconnectedWorkspaces":value["disconnectedWorkspaces"]});
    }
    json!({"workspace":value["workspace"],"language":value["language"],"ready":value["ready"],"status":value["status"],"analyzers":value["analyzers"],"analyzerPid":value["analyzerPid"],"companionConnected":value["companion"].is_string(),"documentCount":value["documents"].as_array().map_or(0, Vec::len),"generation":value["generation"],"checkFresh":value["checkFresh"],"checkRunning":value["check"]["running"],"checkSuccess":value["check"]["success"],"checkError":value["check"]["error"],"watcherError":value["watcherError"]})
}
pub fn diagnostics(mut value: Value, verbose: bool) -> Value {
    if verbose {
        return value;
    }
    if let Some(live) = value["liveDiagnostics"].as_object_mut() {
        live.retain(|_, entry| {
            entry["diagnostics"]
                .as_array()
                .is_some_and(|d| !d.is_empty())
        });
    }
    value["liveDiagnosticCount"] = json!(value["liveDiagnostics"].as_object().map_or(0, |items| {
        items
            .values()
            .map(|v| v["diagnostics"].as_array().map_or(0, Vec::len))
            .sum::<usize>()
    }));
    value["savedDiagnosticCount"] = json!(
        value["savedFileCheck"]["diagnostics"]
            .as_array()
            .map_or(0, Vec::len)
    );
    // Preserve success=null, check error/running/generation and freshness: zero != checked clean.
    if let Some(object) = value.as_object_mut() {
        object.remove("note");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_diagnostics_preserve_uncertainty_and_failures() {
        for check in [
            json!({"success":null,"running":false,"error":null,"diagnostics":[]}),
            json!({"success":null,"running":false,"error":"compiler unavailable","diagnostics":[]}),
            json!({"success":false,"running":false,"error":null,"diagnostics":[{"message":"bad"}]}),
        ] {
            let raw = json!({"ready":true,"checkFresh":false,"savedFileCheck":check,"liveDiagnostics":{"clean":{"diagnostics":[]},"bad":{"diagnostics":[{"message":"overlay error"}],"matchesDocumentVersion":false}}});
            let compact = diagnostics(raw.clone(), false);
            assert_eq!(compact["savedFileCheck"], check);
            assert_eq!(compact["checkFresh"], false);
            assert!(compact["liveDiagnostics"].get("clean").is_none());
            assert_eq!(compact["liveDiagnosticCount"], 1);
            assert_eq!(diagnostics(raw.clone(), true), raw);
        }
    }
}
