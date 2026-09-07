//! Exercise native frontends' shared event listener over real gRPC connections.
#![cfg(unix)]

use std::{sync::Arc, time::Duration};

use cterm_client::{DaemonEventListener, DaemonEventTarget};
use cterm_headless::{
    proto::{
        daemon_event::Event,
        terminal_service_server::{TerminalService, TerminalServiceServer},
        DaemonEvent, SessionMetadataChangedEvent, StreamDaemonEventsRequest,
    },
    service::TerminalServiceImpl,
    session::{SessionManager, SessionState},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};
use tokio_stream::{wrappers::TcpListenerStream, StreamExt};
use tonic::{transport::Server, Request};

struct TestDaemon {
    sessions: Arc<SessionManager>,
    address: std::net::SocketAddr,
    task: JoinHandle<()>,
}

impl TestDaemon {
    async fn start() -> Self {
        let incoming = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = incoming.local_addr().unwrap();
        let sessions = Arc::new(SessionManager::new());
        let service = TerminalServiceImpl::new(sessions.clone(), Arc::new(Notify::new()));
        let task = tokio::spawn(async move {
            Server::builder()
                .add_service(TerminalServiceServer::new(service))
                .serve_with_incoming(TcpListenerStream::new(incoming))
                .await
                .unwrap();
        });
        Self {
            sessions,
            address,
            task,
        }
    }

    fn session(&self) -> Arc<SessionState> {
        self.sessions
            .create_session(
                80,
                24,
                std::env::var("SHELL").ok(),
                vec![],
                None,
                vec![],
                None,
            )
            .unwrap()
    }

    async fn stop(self) {
        for session in self.sessions.list_sessions() {
            self.sessions
                .destroy_session(&session.id, None)
                .await
                .unwrap();
        }
        self.task.abort();
    }
}

async fn next(listener: &mut DaemonEventListener) -> Event {
    tokio::time::timeout(Duration::from_secs(10), listener.next_event())
        .await
        .expect("listener did not recover")
        .1
        .event
        .unwrap()
}

#[tokio::test]
async fn lagging_subscriber_receives_an_explicit_error() {
    let sessions = Arc::new(SessionManager::new());
    let service = TerminalServiceImpl::new(sessions.clone(), Arc::new(Notify::new()));
    let mut events = service
        .stream_daemon_events(Request::new(StreamDaemonEventsRequest {}))
        .await
        .unwrap()
        .into_inner();
    // Do not poll the stream until its bounded broadcast buffer has overflowed.
    for _ in 0..1024 {
        sessions.broadcast_daemon_event(DaemonEvent {
            event: Some(Event::SessionMetadataChanged(
                SessionMetadataChangedEvent::default(),
            )),
        });
    }
    let error = events.next().await.unwrap().unwrap_err();
    assert_eq!(error.code(), tonic::Code::DataLoss);
}

#[tokio::test]
async fn event_buffer_overflow_refreshes_current_metadata() {
    let daemon = TestDaemon::start().await;
    let session = daemon.session();
    let mut listener =
        DaemonEventListener::new(DaemonEventTarget::Tcp(format!("http://{}", daemon.address)));
    assert!(matches!(
        next(&mut listener).await,
        Event::SessionCreated(_)
    ));
    // This current-thread runtime cannot drain the server's stream while the
    // synchronous burst runs. The listener must recover from real gRPC DataLoss.
    for index in 0..1024 {
        session.set_custom_title(format!("title {index}"));
        daemon.sessions.broadcast_session_metadata_changed(&session);
    }
    let Event::SessionMetadataChanged(event) = next(&mut listener).await else {
        panic!("expected refreshed metadata")
    };
    assert_eq!(event.custom_title, "title 1023");
    drop(listener);
    daemon.stop().await;
}

#[tokio::test]
async fn reconnect_reconciles_missed_creates_removals_and_metadata() {
    let daemon = TestDaemon::start().await;
    let removed = daemon.session();
    let retained = daemon.session();

    // A disposable TCP relay lets us interrupt exactly this client's transport
    // while the real daemon and its PTYs remain alive.
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let daemon_address = daemon.address;
    let (relays_tx, mut relays_rx) = tokio::sync::mpsc::unbounded_channel();
    let proxy_task = tokio::spawn(async move {
        loop {
            let (mut client, _) = proxy.accept().await.unwrap();
            let mut upstream = TcpStream::connect(daemon_address).await.unwrap();
            let relay = tokio::spawn(async move {
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
            if relays_tx.send(relay.abort_handle()).is_err() {
                relay.abort();
                break;
            }
        }
    });
    let mut listener =
        DaemonEventListener::new(DaemonEventTarget::Tcp(format!("http://{proxy_address}")));
    let mut initial = std::collections::HashSet::new();
    for _ in 0..2 {
        let Event::SessionCreated(event) = next(&mut listener).await else {
            panic!("expected snapshot create")
        };
        initial.insert(event.session.unwrap().session_id);
    }
    assert_eq!(
        initial,
        [removed.id.clone(), retained.id.clone()]
            .into_iter()
            .collect()
    );

    relays_rx.recv().await.unwrap().abort();
    daemon
        .sessions
        .destroy_session(&removed.id, None)
        .await
        .unwrap();
    retained.set_custom_title("changed while disconnected".into());
    retained.set_tab_color("#123456".into());
    daemon
        .sessions
        .broadcast_session_metadata_changed(&retained);
    let created = daemon.session();

    let mut recovered = std::collections::HashSet::new();
    for _ in 0..3 {
        match next(&mut listener).await {
            Event::SessionCreated(event) => {
                assert_eq!(event.session.unwrap().session_id, created.id);
                assert!(recovered.insert("created"));
            }
            Event::SessionDestroyed(event) => {
                assert_eq!(event.session_id, removed.id);
                assert!(recovered.insert("removed"));
            }
            Event::SessionMetadataChanged(event) => {
                assert_eq!(event.session_id, retained.id);
                assert_eq!(event.custom_title, "changed while disconnected");
                assert_eq!(event.tab_color, "#123456");
                assert!(recovered.insert("metadata"));
            }
        }
    }

    retained.set_custom_title("live after recovery".into());
    daemon
        .sessions
        .broadcast_session_metadata_changed(&retained);
    let Event::SessionMetadataChanged(event) = next(&mut listener).await else {
        panic!("expected live metadata")
    };
    assert_eq!(event.custom_title, "live after recovery");

    drop(listener);
    proxy_task.abort();
    while let Ok(relay) = relays_rx.try_recv() {
        relay.abort();
    }
    daemon.stop().await;
}
