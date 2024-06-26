pub mod completion;
mod config;
mod extraction;
mod file_utils;
mod parser;

use std::path::PathBuf;

use config::Args;
use dashmap::DashMap;
use extraction::ExtractionKind;
use parser::fragemnt::Fragment;
use parser::route::Route;
use ropey::Rope;
use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tree_sitter::Point;

use crate::parser::include::QuteInclude;
use clap::Parser;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if args.get_routes {
        let routes = parser::route::scan_routes();
        let Ok(strout) = serde_json::to_string(&routes) else {
            eprintln!("There was an error converting the data to json");
            return;
        };
        println!("{}", strout);
        return;
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend {
        client,
        document_map: DashMap::new(),
        fragment_map: DashMap::new(),
        route_map: DashMap::new(),
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[derive(Debug)]
struct Backend {
    client: Client,
    document_map: DashMap<String, Rope>,
    fragment_map: DashMap<String, Fragment>,
    route_map: DashMap<String, Route>,
}
impl Backend {
    async fn on_change(&self, params: TextDocumentItem) {
        let rope = ropey::Rope::from_str(&params.text);
        self.document_map
            .insert(params.uri.to_string(), rope.clone());
    }

    fn _get_opened_document(
        &self,
        uri: &Url,
    ) -> Option<dashmap::mapref::one::Ref<'_, std::string::String, Rope>> {
        // when file is open
        if let Some(document) = self.document_map.get(uri.as_str()) {
            return Some(document);
        };
        None
    }

    async fn get_document(
        &self,
        uri: &Url,
    ) -> Option<dashmap::mapref::one::Ref<'_, std::string::String, Rope>> {
        // when file is open
        if let Some(document) = self._get_opened_document(uri) {
            return Some(document);
        };

        let Ok(text) = std::fs::read_to_string(uri.path()) else {
            eprintln!("Unable to open file and it is also not available on the client");
            return None;
        };

        // The file was no opened yet on the client so we have to open it.
        self.on_change(TextDocumentItem {
            uri: uri.clone(),
            text,
            version: 1,
            language_id: "".to_owned(),
        })
        .await;

        // The file should now be loaded
        if let Some(document) = self._get_opened_document(uri) {
            return Some(document);
        };
        None
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                        ..CodeActionOptions::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(
                        [' ', '{', '#', '!', '/']
                            .iter()
                            .map(|i| i.to_string())
                            .collect(),
                    ),
                    ..CompletionOptions::default()
                }),
                ..ServerCapabilities::default()
            },
            server_info: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let fragments = parser::fragemnt::scan_templates();
        for fragemnt in fragments {
            self.fragment_map.insert(fragemnt.id.clone(), fragemnt);
        }
        let routes = parser::route::scan_routes();
        for route in routes {
            self.route_map.insert(route.path.clone(), route);
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.on_change(TextDocumentItem {
            uri: params.text_document.uri,
            text: params.text_document.text,
            version: params.text_document.version,
            language_id: params.text_document.language_id,
        })
        .await
    }

    async fn did_change(&self, mut params: DidChangeTextDocumentParams) {
        self.on_change(TextDocumentItem {
            uri: params.text_document.uri,
            text: std::mem::take(&mut params.content_changes[0].text),
            version: params.text_document.version,
            language_id: "".to_owned(),
        })
        .await
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let params = params.text_document_position;
        let uri = params.text_document.uri;
        let position = params.position;
        let Some(document) = self.get_document(&uri).await else {
            eprintln!("Document is not opened.");
            return Ok(None);
        };
        let Some(line) = document.get_line(position.line.try_into().unwrap_or_default()) else {
            eprintln!("Unable to read the line referecned");
            return Ok(None);
        };
        let mut out = vec![];
        let route_completion = parser::route_completion::completion(
            &self.route_map,
            line.as_str().unwrap_or_default(),
            position.character as usize,
        );
        out.extend(completion::completion(
            line.to_string(),
            position.character as usize,
        ));
        out.extend(parser::fragemnt::completion(
            &self.fragment_map,
            line.to_string(),
            position.character as usize,
        ));
        out.extend(route_completion);
        Ok(Some(CompletionResponse::Array(out)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let params = params.text_document_position_params;
        let uri = params.text_document.uri;
        let position = params.position;
        let Some(document) = self.get_document(&uri).await else {
            eprintln!("Document is not opened.");
            return Ok(None);
        };
        let Some(line) = document.get_line(position.line.try_into().unwrap_or_default()) else {
            eprintln!("Unable to read the line referecned");
            return Ok(None);
        };
        if let Some(definition) = parser::route_definiton::get_definition(
            &self.route_map,
            line.as_str().unwrap_or_default(),
            &position,
        ) {
            return Ok(Some(definition));
        }

        if let Some(include) = parser::include::parse_include(line.to_string()) {
            match include {
                QuteInclude::Basic(reference) => {
                    return Ok(reverence_to_gotodefiniton(&reference));
                }
                QuteInclude::Fragment(fragment) => {
                    let reference = fragment.template;
                    return Ok(reverence_to_gotodefiniton(&reference));
                }
            }
        }

        Ok(None)
    }
    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let Some(document) = self.get_document(&params.text_document.uri).await else {
            eprintln!("Document is not opened.");
            return Ok(None);
        };
        let point = params.range.start;
        let point = Point {
            row: point.line.try_into().unwrap_or_default(),
            column: point.character.try_into().unwrap_or_default(),
        };
        let arguments = Some(vec![
            Value::String(params.text_document.uri.to_string()),
            Value::Number(point.row.into()),
            Value::Number(point.column.into()),
        ]);
        let extract_options: Vec<CodeActionOrCommand> =
            extraction::check_extract(&document.to_string(), point)
                .iter()
                .map(|kind| match kind {
                    ExtractionKind::AddFragment => CodeActionOrCommand::Command(Command {
                        title: "Add fragment frame".to_string(),
                        command: kind.to_string(),
                        arguments: arguments.clone(),
                    }),
                    ExtractionKind::ExtractAsFile => CodeActionOrCommand::Command(Command {
                        title: "Extract as file".to_string(),
                        command: kind.to_string(),
                        arguments: arguments.clone(),
                    }),
                    ExtractionKind::ExtractAsFragment => CodeActionOrCommand::Command(Command {
                        title: "Extract as fragment".to_string(),
                        command: kind.to_string(),
                        arguments: arguments.clone(),
                    }),
                })
                .collect();

        if !extract_options.is_empty() {
            return Ok(Some(extract_options));
        }
        Ok(None)
    }
    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        let (point, uri) = parser::commandargs::parse(params.clone());
        let Some(url) = uri else {
            return Ok(None);
        };
        let Some(document) = self.get_document(&url).await else {
            eprintln!("Document is not opened.");
            return Ok(None);
        };
        let changes = match params.command.parse::<ExtractionKind>() {
            Ok(ExtractionKind::AddFragment) => {
                match extraction::add_fragment(url, point, &document.to_string()) {
                    Ok(changes) => Some(changes),
                    Err(e) => {
                        eprintln!("There was an error while running action AddFragment, {e:?}");
                        None
                    }
                }
            }
            Ok(ExtractionKind::ExtractAsFile) => {
                match extraction::extract_as_file(url, point, &document.to_string()) {
                    Ok(changes) => Some(changes),
                    Err(e) => {
                        eprintln!("There was an error while running action AddFragment, {e:?}");
                        None
                    }
                }
            }
            Ok(ExtractionKind::ExtractAsFragment) => {
                match extraction::extract_as_fragment(url, point, &document.to_string()) {
                    Ok(changes) => Some(changes),
                    Err(e) => {
                        eprintln!("There was an error while running action AddFragment, {e:?}");
                        None
                    }
                }
            }
            Err(_) => None,
        };

        if let Some(changes) = changes {
            if let Err(e) = tokio::time::timeout(
                std::time::Duration::from_millis(10),
                self.client.apply_edit(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
            )
            .await
            {
                eprintln!("Did not get a response from lsp client: {e:?}");
            }
        };

        Ok(None)
    }
}

fn reverence_to_gotodefiniton(reference: &str) -> Option<GotoDefinitionResponse> {
    let Some(path) = template_reverence_to_path(reference) else {
        eprintln!("Unable to get canonicalized path");
        return None;
    };
    let Ok(uri) = Url::from_file_path(path) else {
        eprintln!("Unable to get url from file path");
        return None;
    };
    Some(GotoDefinitionResponse::Scalar(Location::new(
        uri,
        Range::default(),
    )))
}
pub static TEMPLATE_FOLDER: &str = "./src/main/resources/templates/";
fn template_reverence_to_path(reverence: &str) -> Option<PathBuf> {
    std::fs::canonicalize::<PathBuf>(format!("{}{}.html", TEMPLATE_FOLDER, reverence).into()).ok()
}
