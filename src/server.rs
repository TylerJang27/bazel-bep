use crate::{
    build_event_stream::BuildEvent,
    google::devtools::build::v1::{
        build_event::Event,
        publish_build_event_server::{PublishBuildEvent, PublishBuildEventServer},
        PublishBuildToolEventStreamRequest, PublishBuildToolEventStreamResponse,
        PublishLifecycleEventRequest, StreamId,
    },
};
use axum::{BoxError, Router};
use bytes::Bytes;
use prost_types::{Any, Timestamp};
use std::{future::Future, net::SocketAddr};
use tokio_stream::StreamExt;
use tonic::{async_trait, transport::Server};
use tracing::{info_span, Instrument};

pub async fn run_server<E: EventHandler + Send + Sync + 'static>(
    handler: E,
    view: Router,
    addr: SocketAddr,
) -> Result<(), BoxError> {
    let app = axum::Router::new();
    let service = PublishBuildEventServer::new(EventServer::new(handler));
    let service = Server::builder().add_service(service).into_router();

    let app = app.merge(service).nest("/view", view);

    Ok(axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?)
}

pub async fn run_async_server<E: AsyncEventHandler + Send + Sync + 'static>(
    handler: E,
    view: Router,
    addr: SocketAddr,
) -> Result<(), BoxError> {
    let app = axum::Router::new();
    let service = PublishBuildEventServer::new(AsyncEventServer::new(handler));
    let service = Server::builder().add_service(service).into_router();

    let app = app.merge(service).nest("/view", view);

    Ok(axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?)
}

pub fn parse_payload(e: Any) -> Result<BuildEvent, prost::DecodeError> {
    let b = Bytes::from(e.value);
    let b: Result<BuildEvent, _> = prost::Message::decode(b);
    b
}

#[derive(Clone)]
pub struct EventServer<H: EventHandler> {
    handler: H,
}

impl<H: EventHandler> EventServer<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[derive(Clone)]
pub struct AsyncEventServer<H: AsyncEventHandler> {
    handler: H,
}

impl<H: AsyncEventHandler> AsyncEventServer<H> {
    pub fn new(handler: H) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl<H: EventHandler + Send + Sync + 'static> PublishBuildEvent for EventServer<H> {
    /// Publish a build event stating the new state of a build (typically from the
    /// build queue). The BuildEnqueued event must be publishd before all other
    /// events for the same build ID.
    ///
    /// The backend will persist the event and deliver it to registered frontend
    /// jobs immediately without batching.
    ///
    /// The commit status of the request is reported by the RPC's util_status()
    /// function. The error code is the canoncial error code defined in
    /// //util/task/codes.proto.
    #[tracing::instrument(skip_all)]
    async fn publish_lifecycle_event(
        &self,
        request: tonic::Request<PublishLifecycleEventRequest>,
    ) -> std::result::Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();

        tracing::trace!("publish_lifecycle_event");

        if let Some(kind) = request.build_event {
            if let Some(evnt) = kind.event {
                if let Some(evnt_kind) = evnt.event {
                    let stream_id = kind.stream_id.unwrap();
                    let sequence_number = kind.sequence_number;
                    let time = evnt.event_time.unwrap();
                    self.handler
                        .on_event(stream_id, sequence_number, time, evnt_kind)
                        .map_err(Into::into)?;
                }
            }
        }
        Ok(tonic::Response::new(()))
    }
    /// Server streaming response type for the PublishBuildToolEventStream method.
    type PublishBuildToolEventStreamStream = tokio_stream::wrappers::ReceiverStream<
        std::result::Result<PublishBuildToolEventStreamResponse, tonic::Status>,
    >;
    /// Publish build tool events belonging to the same stream to a backend job
    /// using bidirectional streaming.
    #[tracing::instrument(skip_all)]
    async fn publish_build_tool_event_stream(
        &self,
        request: tonic::Request<tonic::Streaming<PublishBuildToolEventStreamRequest>>,
    ) -> std::result::Result<tonic::Response<Self::PublishBuildToolEventStreamStream>, tonic::Status>
    {
        tracing::trace!("publish_build_tool_event_stream");
        let mut request = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(500);
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let this = self.clone();
        tokio::spawn(
            async move {
                let mut init = false;
                while let Some(Ok(evnt)) = request.next().await {
                    let ordered = evnt.ordered_build_event.unwrap();
                    let stream_id = ordered.stream_id.unwrap();
                    if !init {
                        tracing::Span::current()
                            .record("stream_id", format!("{}", stream_id.build_id));
                        init = true;
                    }
                    let sequence_number = ordered.sequence_number;
                    tracing::trace!("evnt {}", stream_id.invocation_id);
                    let res = match this.handler.on_event(
                        stream_id,
                        sequence_number,
                        None,
                        ordered.event.unwrap().event.unwrap(),
                    ) {
                        Ok(id) => Ok(PublishBuildToolEventStreamResponse {
                            stream_id: Some(id),
                            sequence_number: ordered.sequence_number,
                        }),
                        Err(err) => Err(err.into()),
                    };

                    _ = tx.send(res).await;
                }
            }
            .instrument(info_span!("", stream_id = tracing::field::Empty)),
        );
        Ok(tonic::Response::new(stream))
    }
}

#[async_trait]
impl<H: AsyncEventHandler + Send + Sync + 'static> PublishBuildEvent for AsyncEventServer<H> {
    /// Publish a build event stating the new state of a build (typically from the
    /// build queue). The BuildEnqueued event must be publishd before all other
    /// events for the same build ID.
    ///
    /// The backend will persist the event and deliver it to registered frontend
    /// jobs immediately without batching.
    ///
    /// The commit status of the request is reported by the RPC's util_status()
    /// function. The error code is the canoncial error code defined in
    /// //util/task/codes.proto.
    async fn publish_lifecycle_event(
        &self,
        request: tonic::Request<PublishLifecycleEventRequest>,
    ) -> std::result::Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();

        if let Some(kind) = request.build_event {
            if let Some(evnt) = kind.event {
                if let Some(evnt_kind) = evnt.event {
                    let stream_id = kind.stream_id.unwrap();
                    let sequence_number = kind.sequence_number;
                    let time = evnt.event_time.unwrap();
                    self.handler
                        .on_event(stream_id, sequence_number, time, evnt_kind)
                        .await
                        .map_err(Into::into)?;
                }
            }
        }
        Ok(tonic::Response::new(()))
    }
    /// Server streaming response type for the PublishBuildToolEventStream method.
    type PublishBuildToolEventStreamStream = tokio_stream::wrappers::ReceiverStream<
        std::result::Result<PublishBuildToolEventStreamResponse, tonic::Status>,
    >;
    /// Publish build tool events belonging to the same stream to a backend job
    /// using bidirectional streaming.
    async fn publish_build_tool_event_stream(
        &self,
        request: tonic::Request<tonic::Streaming<PublishBuildToolEventStreamRequest>>,
    ) -> std::result::Result<tonic::Response<Self::PublishBuildToolEventStreamStream>, tonic::Status>
    {
        let mut request = request.into_inner();
        let (tx, rx) = tokio::sync::mpsc::channel(500);
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(Ok(evnt)) = request.next().await {
                let ordered = evnt.ordered_build_event.unwrap();
                let stream_id = ordered.stream_id.unwrap();
                let sequence_number = ordered.sequence_number;

                let res = match this
                    .handler
                    .on_event(
                        stream_id,
                        sequence_number,
                        None,
                        ordered.event.unwrap().event.unwrap(),
                    )
                    .await
                {
                    Ok(id) => Ok(PublishBuildToolEventStreamResponse {
                        stream_id: Some(id),
                        sequence_number: ordered.sequence_number,
                    }),
                    Err(err) => Err(err.into()),
                };

                _ = tx.send(res).await;
            }
        });
        Ok(tonic::Response::new(stream))
    }
}

pub trait EventHandler: Clone {
    type Error: Into<tonic::Status>;
    fn on_event(
        &self,
        stream_id: StreamId,
        sequence_number: i64,
        time: impl Into<Option<Timestamp>>,
        e: Event,
    ) -> Result<StreamId, Self::Error>;
}

pub trait AsyncEventHandler: Clone {
    type Error: Into<tonic::Status>;
    fn on_event(
        &self,
        stream_id: StreamId,
        sequence_number: i64,
        time: impl Into<Option<Timestamp>>,
        e: Event,
    ) -> impl Future<Output = Result<StreamId, Self::Error>> + Send + Sync + '_;
}
