// gRPC server — maps all Mimir MCP tools to protobuf RPCs.
// Enabled via "grpc" feature flag. Compiles the proto in build.rs.

#[cfg(feature = "grpc")]
pub mod grpc {
    tonic::include_proto!("mimir.v1");

    use std::sync::{Arc, Mutex};
    use tonic::{Request, Response, Status, Streaming};

    use crate::db::Database;
    use crate::models;

    pub struct MimirGrpcServer {
        db: Arc<Mutex<Database>>,
    }

    impl MimirGrpcServer {
        pub fn new(db: Arc<Mutex<Database>>) -> Self {
            Self { db }
        }
    }

    // Helper to run DB operations inside the mutex
    fn with_db<T>(
        server: &MimirGrpcServer,
        f: impl FnOnce(&Database) -> Result<T, Box<dyn std::error::Error>>,
    ) -> Result<T, Status> {
        let db = server.db.lock().map_err(|_| Status::internal("lock poisoned"))?;
        f(&db).map_err(|e| Status::internal(e.to_string()))
    }

    #[tonic::async_trait]
    impl mimir_server::Mimir for MimirGrpcServer {
        // ── CRUD ──
        async fn remember(&self, req: Request<RememberRequest>) -> Result<Response<RememberResponse>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                let entity = models::Entity {
                    id: String::new(),
                    category: r.category,
                    key: r.key,
                    body_json: r.body_json,
                    status: r.status,
                    entity_type: r.r#type,
                    tags: r.tags,
                    decay_score: r.importance,
                    retrieval_count: 0,
                    layer: "buffer".to_string(),
                    topic_path: r.topic_path,
                    archived: false,
                    archive_reason: String::new(),
                    links: vec![],
                    verified: false,
                    source: "grpc".to_string(),
                    always_on: r.always_on,
                    certainty: r.certainty,
                    workspace_hash: r.workspace_hash,
                    agent_id: r.agent_id,
                    visibility: r.visibility,
                    created_at_unix_ms: crate::db::now_ms(),
                    last_accessed_unix_ms: crate::db::now_ms(),
                    embedding: None,
                };
                let (id, action) = db.remember(&entity)?;
                Ok(Response::new(RememberResponse { id, action, category: entity.category, key: entity.key }))
            })
        }

        async fn recall(&self, req: Request<RecallRequest>) -> Result<Response<RecallResponse>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                let params = models::RecallParams {
                    query: r.query,
                    category: r.category,
                    entity_type: r.r#type,
                    limit: r.limit,
                    offset: r.offset,
                    min_decay: r.min_decay,
                    topic_path: r.topic_path,
                    include_archived: r.include_archived,
                    skip_side_effects: true,
                    mode: crate::models::SearchMode::Fts5,
                    embedding: None,
                    preview_cap: r.preview_cap,
                    always_on: r.always_on,
                    content_weight: r.content_weight,
                    diversity_halving: r.diversity_halving,
                    diversity_per_query_share: 0.0,
                    workspace_hash: r.workspace_hash,
                    agent_id: r.agent_id,
                    visibility: r.visibility,
                };
                let entities = db.recall(&params)?;
                let items = entities.into_iter().map(|e| entity_to_proto(&e)).collect();
                Ok(Response::new(RecallResponse { items, total: items.len() as i64 }))
            })
        }

        async fn get_entity(&self, req: Request<GetEntityRequest>) -> Result<Response<EntityMessage>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                let entity = db.get_entity_by_id(&r.id)
                    .map_err(|_| Status::not_found("entity not found"))?
                    .ok_or_else(|| Status::not_found("entity not found"))?;
                Ok(Response::new(entity_to_proto(&entity)))
            })
        }

        async fn forget(&self, req: Request<ForgetRequest>) -> Result<Response<ForgetResponse>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                db.forget(&r.category, &r.key, &r.reason)?;
                Ok(Response::new(ForgetResponse { ok: true }))
            })
        }

        // ── Graph ──
        async fn link(&self, _req: Request<LinkRequest>) -> Result<Response<LinkResponse>, Status> {
            Err(Status::unimplemented("link"))
        }
        async fn unlink(&self, _req: Request<UnlinkRequest>) -> Result<Response<UnlinkResponse>, Status> {
            Err(Status::unimplemented("unlink"))
        }
        async fn traverse(&self, _req: Request<TraverseRequest>) -> Result<Response<TraverseResponse>, Status> {
            Err(Status::unimplemented("traverse"))
        }

        // ── Journal ──
        async fn journal(&self, req: Request<JournalRequest>) -> Result<Response<JournalEvent>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                let event = models::JournalEvent {
                    id: format!("jrn-{}", uuid::Uuid::new_v4().to_string().replace('-', "").chars().take(12).collect::<String>()),
                    event_type: r.event_type,
                    evaluated_json: r.evaluated_json,
                    acted_json: r.acted_json,
                    forward_json: r.forward_json,
                    category: r.category,
                    key: r.key,
                    entity_id: r.entity_id,
                    agent_id: r.agent_id,
                    created_at_unix_ms: crate::db::now_ms(),
                };
                db.journal(&event)?;
                Ok(Response::new(journal_event_to_proto(&event)))
            })
        }

        async fn timeline(&self, _req: Request<TimelineRequest>) -> Result<Response<TimelineResponse>, Status> {
            Err(Status::unimplemented("timeline"))
        }

        // ── State ──
        async fn state_set(&self, req: Request<StateSetRequest>) -> Result<Response<StateSetResponse>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                db.state_set(&r.key, &r.value_json, r.ttl_seconds.map(|t| t as i64))?;
                Ok(Response::new(StateSetResponse { ok: true }))
            })
        }
        async fn state_get(&self, _req: Request<StateGetRequest>) -> Result<Response<StateEntry>, Status> {
            Err(Status::unimplemented("state_get"))
        }
        async fn state_delete(&self, _req: Request<StateDeleteRequest>) -> Result<Response<StateDeleteResponse>, Status> {
            Err(Status::unimplemented("state_delete"))
        }
        async fn state_list(&self, _req: Request<StateListRequest>) -> Result<Response<StateListResponse>, Status> {
            Err(Status::unimplemented("state_list"))
        }

        // ── Ops ──
        async fn health(&self, _req: Request<HealthRequest>) -> Result<Response<HealthResponse>, Status> {
            with_db(self, |db| {
                db.health()?;
                Ok(Response::new(HealthResponse { healthy: true }))
            })
        }
        async fn stats(&self, _req: Request<StatsRequest>) -> Result<Response<StatsResponse>, Status> {
            with_db(self, |db| {
                let s = db.stats()?;
                Ok(Response::new(StatsResponse {
                    total_entities: s.total_entities,
                    total_journal: s.total_journal,
                    total_state: s.total_state,
                    db_size_bytes: s.db_size_bytes,
                }))
            })
        }
        async fn context(&self, req: Request<ContextRequest>) -> Result<Response<ContextResponse>, Status> {
            let r = req.into_inner();
            with_db(self, |db| {
                let ctx = db.context(&r.categories, r.limit)?;
                Ok(Response::new(ContextResponse { context: ctx }))
            })
        }
        async fn workspace_list(&self, _req: Request<WorkspaceListRequest>) -> Result<Response<WorkspaceListResponse>, Status> {
            with_db(self, |db| {
                let cats = db.workspace_list_categories()?;
                Ok(Response::new(WorkspaceListResponse { categories: cats }))
            })
        }

        // ── AI ──
        async fn ask(&self, _req: Request<AskRequest>) -> Result<Response<AskResponse>, Status> { Err(Status::unimplemented("ask")) }
        async fn embed(&self, _req: Request<EmbedRequest>) -> Result<Response<EmbedResponse>, Status> { Err(Status::unimplemented("embed")) }
        async fn cohere(&self, _req: Request<CohereRequest>) -> Result<Response<CohereResponse>, Status> { Err(Status::unimplemented("cohere")) }

        // ── Lifecycle ──
        async fn decay(&self, _req: Request<DecayRequest>) -> Result<Response<DecayResponse>, Status> { Err(Status::unimplemented("decay")) }
        async fn prune(&self, _req: Request<PruneRequest>) -> Result<Response<PruneResponse>, Status> { Err(Status::unimplemented("prune")) }
        async fn compact(&self, _req: Request<CompactRequest>) -> Result<Response<CompactResponse>, Status> { Err(Status::unimplemented("compact")) }
        async fn score(&self, _req: Request<ScoreRequest>) -> Result<Response<ScoreResponse>, Status> { Err(Status::unimplemented("score")) }

        // ── Quality ──
        async fn conflicts(&self, _req: Request<ConflictsRequest>) -> Result<Response<ConflictsResponse>, Status> { Err(Status::unimplemented("conflicts")) }

        // ── Vault ──
        async fn vault_export(&self, _req: Request<VaultExportRequest>) -> Result<Response<VaultExportResponse>, Status> { Err(Status::unimplemented("vault_export")) }
        async fn vault_import(&self, _req: Request<VaultImportRequest>) -> Result<Response<VaultImportResponse>, Status> { Err(Status::unimplemented("vault_import")) }

        // ── Federation ──
        async fn federate(&self, _req: Request<FederateRequest>) -> Result<Response<FederateResponse>, Status> { Err(Status::unimplemented("federate")) }
        async fn share(&self, _req: Request<ShareRequest>) -> Result<Response<ShareResponse>, Status> { Err(Status::unimplemented("share")) }

        // ── Streaming ──
        type WatchJournalStream = tokio_stream::wrappers::ReceiverStream<Result<JournalEvent, Status>>;
        async fn watch_journal(&self, _req: Request<WatchJournalRequest>) -> Result<Response<Self::WatchJournalStream>, Status> {
            Err(Status::unimplemented("watch_journal"))
        }
        type StreamContextStream = tokio_stream::wrappers::ReceiverStream<Result<ContextChunk, Status>>;
        async fn stream_context(&self, _req: Request<StreamContextRequest>) -> Result<Response<Self::StreamContextStream>, Status> {
            Err(Status::unimplemented("stream_context"))
        }
    }

    // ── Helpers ──
    fn entity_to_proto(e: &models::Entity) -> EntityMessage {
        EntityMessage {
            id: e.id.clone(), category: e.category.clone(), key: e.key.clone(),
            body_json: e.body_json.clone(), status: e.status.clone(), r#type: e.entity_type.clone(),
            tags: e.tags.clone(), decay_score: e.decay_score, retrieval_count: e.retrieval_count,
            layer: e.layer.clone(), topic_path: e.topic_path.clone(),
            archived: e.archived, archive_reason: e.archive_reason.clone(),
            verified: e.verified, source: e.source.clone(), always_on: e.always_on,
            certainty: e.certainty, workspace_hash: e.workspace_hash.clone(),
            agent_id: e.agent_id.clone(), visibility: e.visibility.clone(),
            created_at_unix_ms: e.created_at_unix_ms, last_accessed_unix_ms: e.last_accessed_unix_ms,
        }
    }

    fn journal_event_to_proto(e: &models::JournalEvent) -> JournalEvent {
        JournalEvent {
            id: e.id.clone(), event_type: e.event_type.clone(),
            evaluated_json: e.evaluated_json.clone(), acted_json: e.acted_json.clone(),
            forward_json: e.forward_json.clone(), category: e.category.clone(),
            key: e.key.clone(), entity_id: e.entity_id.clone(),
            agent_id: e.agent_id.clone(), created_at_unix_ms: e.created_at_unix_ms,
        }
    }

    /// Start the gRPC server on the given address. Runs in the current thread
    /// and blocks until shutdown. For background usage, spawn via std::thread::spawn.
    pub async fn serve(
        db: Arc<Mutex<Database>>,
        addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tonic::transport::Server;
        let svc = MimirGrpcServer::new(db);
        Server::builder()
            .add_service(mimir_server::MimirServer::new(svc))
            .serve(addr)
            .await?;
        Ok(())
    }
}

// Non-grpc fallback
#[cfg(not(feature = "grpc"))]
pub mod grpc {
    use std::sync::{Arc, Mutex};
    use crate::db::Database;

    /// Stub module — gRPC is compiled out.
    pub async fn serve(
        _db: Arc<Mutex<Database>>,
        _addr: std::net::SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Err("gRPC transport not compiled in. Rebuild with: cargo build --features grpc".into())
    }
}
