use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use tokio::net::TcpListener;
use tokio::sync;

use gluesql::prelude::*;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::query::{PlaceholderExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::{ClientInfo, MakeHandler, StatelessMakeHandler, Type};
use pgwire::error::{PgWireError, PgWireResult};
use pgwire::tokio::process_socket;

pub struct GluesqlProcessor {
    tx: Arc<sync::mpsc::Sender<(sync::oneshot::Sender<Result<Vec<Payload>>>, String)>>
}

#[async_trait]
impl SimpleQueryHandler for GluesqlProcessor {
    async fn do_query<'a, C>(
        &self,
        _client: &mut C,
        query: &'a str,
    ) -> PgWireResult<Vec<Response<'a>>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        println!("{:?}", query);
        let (tx, rx) = sync::oneshot::channel();
        self.tx.send((tx, String::from(query))).await.map_err(|err| PgWireError::ApiError(Box::new(err)))?;
        let payloads = rx.await.map_err(|err| PgWireError::ApiError(Box::new(err)))?;
        payloads.map_err(|err| PgWireError::ApiError(Box::new(err)))
            .and_then(|payloads| {
                payloads
                    .into_iter()
                    .map(|payload| match payload {
                        Payload::Select { labels, rows } => {
                            let fields = labels
                                .iter()
                                .map(|label| {
                                    FieldInfo::new(
                                        label.into(),
                                        None,
                                        None,
                                        Type::UNKNOWN,
                                        FieldFormat::Text,
                                    )
                                })
                                .collect::<Vec<_>>();
                            let fields = Arc::new(fields);

                            let mut results = Vec::with_capacity(rows.len());
                            for row in rows {
                                let mut encoder = DataRowEncoder::new(fields.clone());
                                for field in row.iter() {
                                    match field {
                                        Value::Bool(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::BOOL,
                                                FieldFormat::Text,
                                            )?,
                                        Value::I8(v) => encoder.encode_field_with_type_and_format(
                                            v,
                                            &Type::CHAR,
                                            FieldFormat::Text,
                                        )?,
                                        Value::I16(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::INT2,
                                                FieldFormat::Text,
                                            )?,
                                        Value::I32(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::INT4,
                                                FieldFormat::Text,
                                            )?,
                                        Value::I64(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::INT8,
                                                FieldFormat::Text,
                                            )?,
                                        Value::U8(v) => encoder.encode_field_with_type_and_format(
                                            &(*v as i8),
                                            &Type::CHAR,
                                            FieldFormat::Text,
                                        )?,
                                        Value::F64(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::FLOAT8,
                                                FieldFormat::Text,
                                            )?,
                                        Value::Str(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::VARCHAR,
                                                FieldFormat::Text,
                                            )?,
                                        Value::Bytea(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::BYTEA,
                                                FieldFormat::Text,
                                            )?,
                                        Value::Date(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::DATE,
                                                FieldFormat::Text,
                                            )?,
                                        Value::Time(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::TIME,
                                                FieldFormat::Text,
                                            )?,
                                        Value::Timestamp(v) => encoder
                                            .encode_field_with_type_and_format(
                                                v,
                                                &Type::TIMESTAMP,
                                                FieldFormat::Text,
                                            )?,
                                        _ => unimplemented!(),
                                    }
                                }
                                results.push(encoder.finish());
                            }

                            Ok(Response::Query(QueryResponse::new(
                                fields,
                                stream::iter(results.into_iter()),
                            )))
                        }
                        Payload::Insert(rows) => Ok(Response::Execution(Tag::new_for_execution(
                            "Insert",
                            Some(rows),
                        ))),
                        Payload::Delete(rows) => Ok(Response::Execution(Tag::new_for_execution(
                            "Delete",
                            Some(rows),
                        ))),
                        Payload::Update(rows) => Ok(Response::Execution(Tag::new_for_execution(
                            "Update",
                            Some(rows),
                        ))),
                        Payload::Create => {
                            Ok(Response::Execution(Tag::new_for_execution("Create", None)))
                        }
                        Payload::AlterTable => Ok(Response::Execution(Tag::new_for_execution(
                            "Alter Table",
                            None,
                        ))),
                        Payload::DropTable => Ok(Response::Execution(Tag::new_for_execution(
                            "Drop Table",
                            None,
                        ))),
                        Payload::CreateIndex => Ok(Response::Execution(Tag::new_for_execution(
                            "Create Index",
                            None,
                        ))),
                        Payload::DropIndex => Ok(Response::Execution(Tag::new_for_execution(
                            "Drop Index",
                            None,
                        ))),
                        _ => {
                            unimplemented!()
                        }
                    })
                    .collect::<Result<Vec<Response>, PgWireError>>()
            })
    }
}

#[tokio::main]
pub async fn main() {
    let (gluetx, mut gluerx) = sync::mpsc::channel(1);
    let processor = Arc::new(GluesqlProcessor { tx: Arc::new(gluetx) });

    // We have not implemented extended query in this server, use placeholder instead
    let placeholder = StatelessMakeHandler::new(Arc::new(
        PlaceholderExtendedQueryHandler,
    ));
    let authenticator = StatelessMakeHandler::new(Arc::new(NoopStartupHandler));

    let server_addr = "127.0.0.1:5432";
    let listener = TcpListener::bind(server_addr).await.unwrap();
    println!("Listening to {}", server_addr);

    tokio::spawn(async move {
        let mut glue = Glue::new(MemoryStorage::default());
        while let Some((tx, sql)) = gluerx.recv().await {
            tx.send(futures::executor::block_on(glue.execute(sql))).ok();
        }
    });

    loop {
        let incoming_socket = listener.accept().await.unwrap();
        let authenticator_ref = authenticator.make();
        let processor_ref = processor.clone();
        let placeholder_ref = placeholder.make();
        tokio::spawn(async move {
            process_socket(
                incoming_socket.0,
                None,
                authenticator_ref,
                processor_ref,
                placeholder_ref,
            )
            .await
        });
    }
}
