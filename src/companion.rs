use crate::{
    core::SyncEvent,
    protocol::{edit_text, read_message, write_message},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::BufReader;

#[derive(Default, Clone)]
struct EditorDocument {
    text: String,
    epoch: u64,
    replay: bool,
    suppress_through: u64,
}
impl EditorDocument {
    fn acknowledge(&mut self, value: &Value) {
        self.epoch = value["epoch"].as_u64().unwrap_or(self.epoch);
        if value["accepted"] == false {
            self.replay = false;
        }
    }
}
pub async fn run(endpoint: String) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut root = String::new();
    let mut connected = false;
    let mut documents: HashMap<String, EditorDocument> = HashMap::new();
    let mut instance = String::new();
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(3));
    // Keep a dedicated reader: canceling read_message mid-frame would corrupt framing.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let received = Arc::new(AtomicU64::new(0));
    let incoming = received.clone();
    tokio::spawn(async move {
        while let Ok(Some(message)) = read_message(&mut input).await {
            let sequence = incoming.fetch_add(1, Ordering::SeqCst) + 1;
            if tx.send((sequence, message)).is_err() {
                break;
            }
        }
    });
    loop {
        let (sequence, message) = tokio::select! {
            message = rx.recv() => match message {Some(v)=>v,None=>break},
            _ = heartbeat.tick(), if !root.is_empty() => {
                let event = SyncEvent {workspace:root.clone(),companion:id.clone(),kind:if connected {"heartbeat"} else {"connect"}.into(),uri:None,text:None,epoch:0};
                match send(&client,&endpoint,event).await {
                    Ok(value) => {
                        if !connected {
                            connected = true;
                            let next_instance = value["instance"].as_str().unwrap_or("");
                            let restarted = instance != next_instance;
                            instance = next_instance.into();
                            for (uri,doc) in &mut documents {
                                if restarted { doc.epoch = 0; }
                                if !doc.replay { continue; }
                                let event = SyncEvent {workspace:root.clone(),companion:id.clone(),kind:"open".into(),uri:Some(uri.clone()),text:Some(doc.text.clone()),epoch:doc.epoch};
                                match send(&client,&endpoint,event).await {Ok(value)=>{doc.acknowledge(&value); if !doc.replay {doc.suppress_through = received.load(Ordering::SeqCst);}},Err(_)=>{connected=false;break;}}
                            }
                        }
                    }
                    Err(error) => { connected = false; tracing::debug!("Companion disconnected: {error}"); }
                }
                continue;
            }
        };
        let method = message["method"].as_str().unwrap_or("");
        let params = &message["params"];
        if let Some(request_id) = message.get("id") {
            let result = match method {
                "initialize" => {
                    let uri = params["rootUri"]
                        .as_str()
                        .or_else(|| params["workspaceFolders"][0]["uri"].as_str())
                        .context("Zed did not provide a workspace root")?;
                    root = url::Url::parse(uri)?
                        .to_file_path()
                        .map_err(|_| anyhow::anyhow!("Invalid root URI"))?
                        .to_string_lossy()
                        .into();
                    json!({"capabilities":{"positionEncoding":"utf-16","textDocumentSync":{"openClose":true,"change":1,"save":{"includeText":true}}},"serverInfo":{"name":"Editor bridge companion","version":env!("CARGO_PKG_VERSION")}})
                }
                "shutdown" => Value::Null,
                _ => {
                    write_message(&mut output,&json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Unsupported method"}})).await?;
                    continue;
                }
            };
            write_message(
                &mut output,
                &json!({"jsonrpc":"2.0","id":request_id,"result":result}),
            )
            .await?;
            continue;
        }
        if method == "exit" {
            break;
        }
        let Some(uri) = params["textDocument"]["uri"].as_str() else {
            continue;
        };
        let kind = match method {
            "textDocument/didOpen" => {
                documents.insert(
                    uri.into(),
                    EditorDocument {
                        text: params["textDocument"]["text"].as_str().unwrap_or("").into(),
                        epoch: 0,
                        replay: true,
                        suppress_through: 0,
                    },
                );
                "open"
            }
            "textDocument/didChange" => {
                if let Some(doc) = documents.get_mut(uri) {
                    doc.replay = sequence > doc.suppress_through;
                    let text = &mut doc.text;
                    for change in params["contentChanges"]
                        .as_array()
                        .context("Missing changes")?
                    {
                        *text = if change.get("range").is_some() {
                            edit_text(
                                text,
                                &[json!({"range":change["range"],"newText":change["text"]})],
                            )?
                        } else {
                            change["text"].as_str().context("Missing text")?.into()
                        };
                    }
                }
                "change"
            }
            "textDocument/didSave" => "save",
            "textDocument/didClose" => "close",
            _ => continue,
        };
        if connected && (kind != "change" || documents.get(uri).is_some_and(|doc| doc.replay)) {
            let doc = documents.get(uri).cloned().unwrap_or_default();
            let event = SyncEvent {
                workspace: root.clone(),
                companion: id.clone(),
                kind: kind.into(),
                uri: Some(uri.into()),
                text: Some(doc.text),
                epoch: doc.epoch,
            };
            match send(&client, &endpoint, event).await {
                Ok(value) => {
                    if let Some(doc) = documents.get_mut(uri) {
                        doc.acknowledge(&value);
                        if !doc.replay {
                            doc.suppress_through = received.load(Ordering::SeqCst);
                        }
                    }
                }
                Err(_) => connected = false,
            }
        }
        if kind == "close" {
            documents.remove(uri);
        }
    }
    if connected {
        let _ = send(
            &client,
            &endpoint,
            SyncEvent {
                workspace: root,
                companion: id,
                kind: "disconnect".into(),
                uri: None,
                text: None,
                epoch: 0,
            },
        )
        .await;
    }
    Ok(())
}
async fn send(client: &reqwest::Client, endpoint: &str, event: SyncEvent) -> Result<Value> {
    let response = client
        .post(format!("{}/api/companion", endpoint.trim_end_matches('/')))
        .json(&event)
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("{status}: {body}");
    }
    Ok(serde_json::from_str(&body)?)
}
