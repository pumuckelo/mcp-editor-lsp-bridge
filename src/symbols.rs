use serde_json::{Value, json};

pub fn flatten(symbols: &Value, uri: &str) -> Vec<Value> {
    fn visit(items: &[Value], parent: Option<&str>, uri: &str, out: &mut Vec<Value>) {
        for item in items {
            let range = item
                .get("selectionRange")
                .or_else(|| item.get("range"))
                .unwrap_or(&item["location"]["range"]);
            out.push(json!({"name":item["name"],"kind":item["kind"],"container":item["containerName"].as_str().or(parent),"uri":item["location"]["uri"].as_str().unwrap_or(uri),"line":range["start"]["line"],"character":range["start"]["character"],"exactSelection":item.get("selectionRange").is_some(),"range":range}));
            if let Some(children) = item["children"].as_array() {
                visit(children, item["name"].as_str(), uri, out);
            }
        }
    }
    let mut out = vec![];
    visit(
        symbols.as_array().map(Vec::as_slice).unwrap_or_default(),
        None,
        uri,
        &mut out,
    );
    out
}
pub fn compact(items: Vec<Value>) -> Value {
    json!(
        items
            .into_iter()
            .map(|mut item| {
                let object = item.as_object_mut().unwrap();
                object.remove("exactSelection");
                object.remove("range");
                object.remove("uri");
                item
            })
            .collect::<Vec<_>>()
    )
}
