use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cogwheel_classifier::{Classification, ClassifierSettings, classify_domain};
use cogwheel_policy::{BlockMode, DecisionKind, PolicyEngine};
use hickory_proto::op::{Message, MessageType, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA};
use hickory_proto::rr::{RData, Record};
use hickory_resolver::TokioResolver;
use moka::future::Cache;
use serde::Serialize;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

#[derive(Debug, Clone)]
pub struct DnsRuntimeConfig {
    pub udp_bind_addr: SocketAddr,
    pub tcp_bind_addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct DnsRuntime {
    resolver: TokioResolver,
    policy: Arc<RwLock<Arc<PolicyEngine>>>,
    classifier_settings: ClassifierSettings,
    cache: Cache<String, CachedLookup>,
    fallback_cache: Cache<String, CachedLookup>,
    stats: Arc<DnsRuntimeStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassificationEvent {
    pub domain: String,
    pub classification: Classification,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct CachedLookup {
    response: Message,
}

#[derive(Debug, Default)]
pub struct DnsRuntimeStats {
    upstream_failures_total: AtomicU64,
    fallback_served_total: AtomicU64,
    cache_hits_total: AtomicU64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DnsRuntimeSnapshot {
    pub upstream_failures_total: u64,
    pub fallback_served_total: u64,
    pub cache_hits_total: u64,
}

impl DnsRuntime {
    pub fn new(
        resolver: TokioResolver,
        policy: Arc<PolicyEngine>,
        classifier_settings: ClassifierSettings,
    ) -> Self {
        Self {
            resolver,
            policy: Arc::new(RwLock::new(policy)),
            classifier_settings,
            cache: Cache::new(10_000),
            fallback_cache: Cache::new(10_000),
            stats: Arc::new(DnsRuntimeStats::default()),
        }
    }

    pub fn replace_policy(&self, policy: Arc<PolicyEngine>) {
        if let Ok(mut guard) = self.policy.write() {
            *guard = policy;
        }
    }

    pub fn snapshot(&self) -> DnsRuntimeSnapshot {
        DnsRuntimeSnapshot {
            upstream_failures_total: self.stats.upstream_failures_total.load(Ordering::Relaxed),
            fallback_served_total: self.stats.fallback_served_total.load(Ordering::Relaxed),
            cache_hits_total: self.stats.cache_hits_total.load(Ordering::Relaxed),
        }
    }

    pub async fn serve(self: Arc<Self>, config: DnsRuntimeConfig) -> Result<()> {
        let udp = tokio::spawn(self.clone().serve_udp(config.udp_bind_addr));
        let tcp = tokio::spawn(self.clone().serve_tcp(config.tcp_bind_addr));
        udp.await??;
        tcp.await??;
        Ok(())
    }

    async fn serve_udp(self: Arc<Self>, bind_addr: SocketAddr) -> Result<()> {
        let socket = UdpSocket::bind(bind_addr)
            .await
            .context("bind udp socket")?;
        let mut buffer = [0u8; 4096];
        loop {
            let (size, peer) = socket.recv_from(&mut buffer).await?;
            let response = self
                .handle_wire_query(&buffer[..size])
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to handle udp dns query");
                    Message::error_msg(0, hickory_proto::op::OpCode::Query, ResponseCode::ServFail)
                });
            let response_bytes = response.to_vec()?;
            socket.send_to(&response_bytes, peer).await?;
        }
    }

    async fn serve_tcp(self: Arc<Self>, bind_addr: SocketAddr) -> Result<()> {
        let listener = TcpListener::bind(bind_addr)
            .await
            .context("bind tcp listener")?;
        loop {
            let (stream, _) = listener.accept().await?;
            let runtime = self.clone();
            tokio::spawn(async move {
                if let Err(error) = runtime.handle_tcp_stream(stream).await {
                    tracing::warn!(%error, "failed to handle tcp dns query");
                }
            });
        }
    }

    async fn handle_tcp_stream(&self, mut stream: TcpStream) -> Result<()> {
        let mut len_buffer = [0u8; 2];
        stream.read_exact(&mut len_buffer).await?;
        let length = u16::from_be_bytes(len_buffer) as usize;
        let mut payload = vec![0u8; length];
        stream.read_exact(&mut payload).await?;
        let response = self.handle_wire_query(&payload).await?;
        let response_bytes = response.to_vec()?;
        stream
            .write_all(&(response_bytes.len() as u16).to_be_bytes())
            .await?;
        stream.write_all(&response_bytes).await?;
        Ok(())
    }

    async fn handle_wire_query(&self, payload: &[u8]) -> Result<Message> {
        let request = Message::from_vec(payload)?;
        let query = request
            .queries()
            .first()
            .cloned()
            .context("dns query missing question")?;
        let name = query.name().to_utf8();
        let domain = name.trim_end_matches('.').to_ascii_lowercase();

        if let Some(classification) = classify_domain(&domain, &self.classifier_settings) {
            tracing::debug!(domain, score = classification.score, "domain classified");
        }

        if let Some(cached) = self.cache.get(&domain).await {
            self.stats.cache_hits_total.fetch_add(1, Ordering::Relaxed);
            return Ok(cached.response);
        }

        let engine = self.policy.read().expect("policy lock poisoned").clone();
        let decision = engine.evaluate(&domain);

        let response = match decision.kind {
            DecisionKind::Blocked(mode) => build_blocked_response(&request, mode),
            DecisionKind::Allowed => match self.resolve_upstream(&request, &domain).await {
                Ok(response) => {
                    self.fallback_cache
                        .insert(
                            domain.clone(),
                            CachedLookup {
                                response: response.clone(),
                            },
                        )
                        .await;
                    response
                }
                Err(error) => {
                    self.stats
                        .upstream_failures_total
                        .fetch_add(1, Ordering::Relaxed);
                    if let Some(fallback) = self.fallback_cache.get(&domain).await {
                        self.stats
                            .fallback_served_total
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(%domain, %error, "serving fallback DNS response after upstream failure");
                        fallback.response
                    } else {
                        return Err(error);
                    }
                }
            },
        };

        self.cache
            .insert(
                domain,
                CachedLookup {
                    response: response.clone(),
                },
            )
            .await;
        Ok(response)
    }

    async fn resolve_upstream(&self, request: &Message, domain: &str) -> Result<Message> {
        let query = request
            .queries()
            .first()
            .context("dns query missing question")?;
        let lookup = self.resolver.lookup(domain, query.query_type()).await?;
        let mut response = build_base_response(request, ResponseCode::NoError);
        for record in lookup.records() {
            response.add_answer(record.clone());
        }
        Ok(response)
    }
}

fn build_base_response(request: &Message, code: ResponseCode) -> Message {
    let mut response = Message::new();
    response.set_id(request.id());
    response.set_message_type(MessageType::Response);
    response.set_op_code(request.op_code());
    response.set_authoritative(false);
    response.set_recursion_desired(request.recursion_desired());
    response.set_recursion_available(true);
    response.set_response_code(code);
    for query in request.queries() {
        response.add_query(query.clone());
    }
    response
}

fn build_blocked_response(request: &Message, mode: BlockMode) -> Message {
    match mode {
        BlockMode::NxDomain => build_base_response(request, ResponseCode::NXDomain),
        BlockMode::NoData => build_base_response(request, ResponseCode::NoError),
        BlockMode::Refused => build_base_response(request, ResponseCode::Refused),
        BlockMode::NullIp => build_ip_response(
            request,
            Some(Ipv4Addr::new(0, 0, 0, 0)),
            Some(Ipv6Addr::UNSPECIFIED),
        ),
        BlockMode::CustomIp { ipv4, ipv6 } => build_ip_response(request, ipv4, ipv6),
    }
}

fn build_ip_response(request: &Message, ipv4: Option<Ipv4Addr>, ipv6: Option<Ipv6Addr>) -> Message {
    let mut response = build_base_response(request, ResponseCode::NoError);
    for query in request.queries() {
        let name = query.name().clone();
        match query.query_type() {
            hickory_proto::rr::RecordType::A => {
                if let Some(address) = ipv4 {
                    response.add_answer(Record::from_rdata(name, 60, RData::A(A(address))));
                }
            }
            hickory_proto::rr::RecordType::AAAA => {
                if let Some(address) = ipv6 {
                    response.add_answer(Record::from_rdata(name, 60, RData::AAAA(AAAA(address))));
                }
            }
            _ => {}
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_snapshot_starts_at_zero() {
        let stats = DnsRuntimeStats::default();
        let snapshot = DnsRuntimeSnapshot {
            upstream_failures_total: stats.upstream_failures_total.load(Ordering::Relaxed),
            fallback_served_total: stats.fallback_served_total.load(Ordering::Relaxed),
            cache_hits_total: stats.cache_hits_total.load(Ordering::Relaxed),
        };
        assert_eq!(
            snapshot,
            DnsRuntimeSnapshot {
                upstream_failures_total: 0,
                fallback_served_total: 0,
                cache_hits_total: 0,
            }
        );
    }
}
