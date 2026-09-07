//! Recoverable daemon-wide session synchronization, shared by native frontends.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use cterm_proto::proto::{
    daemon_event::Event, DaemonEvent, SessionCreatedEvent, SessionDestroyedEvent, SessionInfo,
    SessionMetadataChangedEvent,
};

use crate::{ClientError, DaemonConnection};

/// The endpoint whose sessions a frontend mirrors.
#[derive(Clone)]
pub enum DaemonEventTarget {
    Local,
    Unix(PathBuf),
    Tcp(String),
}

impl DaemonEventTarget {
    async fn connect(&self) -> Result<DaemonConnection, ClientError> {
        match self {
            Self::Local => DaemonConnection::connect_local().await,
            Self::Unix(path) => DaemonConnection::connect_unix(path, false).await,
            Self::Tcp(address) => DaemonConnection::connect_tcp(address).await,
        }
    }
}

/// Reconnects after EOF, transport errors, or a daemon event-buffer overflow.
/// Each subscription refreshes the session list to recover missed creations,
/// removals and metadata changes. Drop this listener (or cancel `next_event`)
/// when its frontend closes. It never owns or destroys daemon sessions.
pub struct DaemonEventListener {
    target: DaemonEventTarget,
    connection: Option<DaemonConnection>,
    stream: Option<tonic::Streaming<DaemonEvent>>,
    known: HashSet<String>,
    pending: VecDeque<DaemonEvent>,
    retry_delay: Duration,
}

impl DaemonEventListener {
    pub fn new(target: DaemonEventTarget) -> Self {
        Self {
            target,
            connection: None,
            stream: None,
            known: HashSet::new(),
            pending: VecDeque::new(),
            retry_delay: Duration::from_millis(250),
        }
    }

    /// Return the next change and its connection (for attaching new tabs).
    /// Connection attempts are bounded and retried with capped backoff. Idle
    /// event streams have no timeout: an unchanged daemon can be quiet forever.
    pub async fn next_event(&mut self) -> (DaemonConnection, DaemonEvent) {
        loop {
            if let Some(event) = self.pending.pop_front() {
                if self.observe(&event) {
                    return (self.connection.as_ref().unwrap().clone(), event);
                }
                continue;
            }

            if let Some(stream) = self.stream.as_mut() {
                match stream.message().await {
                    Ok(Some(event)) => {
                        self.retry_delay = Duration::from_millis(250);
                        if self.observe(&event) {
                            return (self.connection.as_ref().unwrap().clone(), event);
                        }
                        continue;
                    }
                    Ok(None) => log::debug!("daemon event stream closed; reconnecting"),
                    Err(error) => log::warn!("daemon event stream failed: {error}; reconnecting"),
                }
                self.stream = None;
                self.connection = None;
                self.backoff().await;
            }

            match tokio::time::timeout(Duration::from_secs(15), self.subscribe()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    log::warn!("daemon event subscription failed: {error}; retrying");
                    self.backoff().await;
                }
                Err(_) => {
                    log::warn!("daemon event subscription timed out; retrying");
                    self.backoff().await;
                }
            }
        }
    }

    async fn backoff(&mut self) {
        tokio::time::sleep(self.retry_delay).await;
        self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(5));
    }

    async fn subscribe(&mut self) -> Result<(), ClientError> {
        let connection = self.target.connect().await?;
        // Subscribe FIRST so changes concurrent with the snapshot remain queued.
        // Replayed creates are deduplicated; subsequent events bring any snapshot
        // metadata up to date. An overflow triggers another complete refresh.
        let stream = connection.stream_daemon_events().await?;
        let sessions = connection.list_sessions().await?;
        self.pending = self.reconcile(sessions);
        self.connection = Some(connection);
        self.stream = Some(stream);
        Ok(())
    }

    fn reconcile(&self, sessions: Vec<SessionInfo>) -> VecDeque<DaemonEvent> {
        let live: HashSet<_> = sessions
            .iter()
            .filter(|s| s.running)
            .map(|s| s.session_id.clone())
            .collect();
        let mut events = VecDeque::new();
        for id in self.known.difference(&live) {
            events.push_back(DaemonEvent {
                event: Some(Event::SessionDestroyed(SessionDestroyedEvent {
                    session_id: id.clone(),
                })),
            });
        }
        for session in sessions.into_iter().filter(|s| s.running) {
            if !self.known.contains(&session.session_id) {
                events.push_back(DaemonEvent {
                    event: Some(Event::SessionCreated(SessionCreatedEvent {
                        session: Some(session),
                    })),
                });
            } else {
                events.push_back(DaemonEvent {
                    event: Some(Event::SessionMetadataChanged(SessionMetadataChangedEvent {
                        session_id: session.session_id,
                        custom_title: session.custom_title,
                        tab_color: session.tab_color,
                        template_name: session.template_name,
                    })),
                });
            }
        }
        events
    }

    fn observe(&mut self, event: &DaemonEvent) -> bool {
        match &event.event {
            Some(Event::SessionCreated(created)) => created
                .session
                .as_ref()
                .is_some_and(|info| info.running && self.known.insert(info.session_id.clone())),
            Some(Event::SessionDestroyed(destroyed)) => {
                // Forward even unknown removals: a frontend may already own the
                // tab before the listener's first subscription has completed.
                self.known.remove(&destroyed.session_id);
                true
            }
            Some(Event::SessionMetadataChanged(_)) => true,
            None => false,
        }
    }
}
