use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
    routing::any,
    Router,
};
use axum_extra::TypedHeader;
use tokio_util::codec::{Framed, LinesCodec};

use std::{
    collections::HashMap,
    error::Error,
    io,
    sync::{mpsc, Mutex},
};
use std::{net::SocketAddr, path::PathBuf};
use std::{ops::ControlFlow, sync::Arc};
use tower_http::{
    services::ServeDir,
    trace::{DefaultMakeSpan, TraceLayer},
};

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

//allows to extract the IP of connecting user
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::CloseFrame;

//allows to split the websocket stream into separate TX and RX branches
use futures::{sink::SinkExt, stream::StreamExt};
use futures_util::stream::{SplitSink, SplitStream};

//#[derive(Clone)]
struct AppState {
    rooms: Mutex<HashMap<String, Room>>,
}
use chrono::prelude::*;
use tokio::net::{tcp::OwnedWriteHalf, TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

    let state = Arc::new(AppState {
        rooms: Mutex::new(HashMap::new()),
    });
    let app = Router::new()
        .fallback_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
        .route("/ws", any(ws_handler))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .with_state(Arc::clone(&state));

    let listener1 = tokio::net::TcpListener::bind("127.0.0.1:3001")
        .await
        .unwrap();
    tracing::debug!("listening on {}", listener1.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            // Asynchronously wait for an inbound TcpStream.
            let (stream, addr) = listener1.accept().await.unwrap();

            // Clone a handle to the `Shared` state for the new connection.
            let state = Arc::clone(&state);

            // Spawn our handler to be run asynchronously.
            tokio::spawn(async move {
                tracing::debug!("accepted connection");
                if let Err(e) = handle_tcp_stream(state, stream, addr).await {
                    tracing::info!("an error occurred; error = {:?}", e);
                }
            });
        }
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::Sender<String>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::Receiver<String>;

/// The state for each connected client.
struct Peer {
    rx: Rx,
}

impl Peer {
    /// Create a new instance of `Peer`.
    fn new(addr: SocketAddr, room_name: &str, state: Arc<AppState>) -> io::Result<Peer> {
        // Create a channel for this peer
        let (tx, rx) = mpsc::channel::<String>();

        // Add an entry for this `Peer` in the shared state map.
        let mut rooms = state.rooms.lock().unwrap();
        let room = rooms.get_mut(room_name).unwrap();
        room.peers.insert(addr, tx);

        Ok(Peer { rx })
    }
}

struct Room {
    peers: HashMap<SocketAddr, Tx>,
    history: Vec<ChatMessage>,
    //tx: Arc<Mutex<Vec<MessageSender>>>,
}
impl Room {
    fn new() -> Self {
        Self {
            peers: HashMap::new(),
            history: vec![],
            //tx: Arc::new(Mutex::new(vec![])),
        }
    }
    fn broadcast(&mut self, sender: SocketAddr, message: &str) {
        for peer in self.peers.iter_mut() {
            if *peer.0 != sender {
                let _ = peer.1.send(message.into());
            }
        }
    }
}
struct ChatMessage {
    text: String,
    sedner_id: i32,
    receiver_id: i32,
    time: DateTime<Utc>,
}
#[derive(Debug)]
enum MessageSender {
    WebSocket(SplitSink<WebSocket, Message>),
    TcpSocket(OwnedWriteHalf),
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
        user_agent.to_string()
    } else {
        String::from("Unknown browser")
    };
    println!("`{user_agent}` at {addr} connected.");
    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket(socket, addr, state))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(mut socket: WebSocket, who: SocketAddr, state: Arc<AppState>) {
    // unsolicited messages to client based on some sort of server's internal event (i.e .timer).
    let (mut sender, mut receiver) = socket.split();

    {
        let mut rooms = state.rooms.lock().unwrap();
        let room = rooms
            .entry("chatroom".to_string())
            .or_insert_with(Room::new);
    }

    let mut peer = Peer::new(who, "chatroom", state.clone());

    let peer = peer.unwrap();
    let state1 = Arc::clone(&state);
    let state2 = Arc::clone(&state);

    {
        let mut rooms = state.rooms.lock().unwrap();
        let msg = format!("{} has joined the chat", who);
        tracing::info!("{}", msg);
        let room = rooms.get_mut("chatroom").unwrap();
        room.broadcast(who, &msg);
    }
    // Spawn a task that will push several messages to the client (does not matter what client does)
    let mut send_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(t) => {
                    println!(">>> {who} sent str: {t:?}");
                    {
                        let rooms = state1.rooms.lock();
                        let mut rooms = rooms.unwrap();
                        let room = rooms.get_mut("chatroom").unwrap();
                        room.broadcast(who, &t);
                    }
                }

                Message::Binary(d) => {
                    println!(">>> {} sent {} bytes: {:?}", who, d.len(), d);
                }
                Message::Close(c) => {
                    if let Some(cf) = c {
                        println!(
                            ">>> {} sent close with code {} and reason `{}`",
                            who, cf.code, cf.reason
                        );
                    } else {
                        println!(">>> {who} somehow sent close message without CloseFrame");
                    }
                    break;
                }

                Message::Pong(v) => {
                    println!(">>> {who} sent pong with {v:?}");
                }
                // You should never need to manually handle Message::Ping, as axum's websocket library
                // will do so for you automagically by replying with Pong and copying the v according to
                // spec. But if you need the contents of the pings you can see them here.
                Message::Ping(v) => {
                    println!(">>> {who} sent ping with {v:?}");
                }
            }
        }
    });

    // This second task will receive messages from client and print them on server console
    let mut recv_task = tokio::spawn(async move {
        while let Ok(msg) = peer.rx.recv() {
            //println!("Got a message: {}", msg);
            if sender.send(Message::Text(msg)).await.is_err() {
                println!("Sending message failed");
                break;
            }
        }
    });

    // If any one of the tasks exit, abort the other.
    tokio::select! {
        _ = (&mut send_task) => {
            recv_task.abort();
        },
        _ = (&mut recv_task) => {
            send_task.abort();
        }
    }

    // returning from the handler closes the websocket connection
    println!("Websocket context {who} destroyed");

    {
        let mut room = state2.rooms.lock().unwrap();
        let room = room.get_mut("chatroom").unwrap();
        room.peers.remove(&who);
        let msg = format!("{} has left the chat", who);
        room.broadcast(who, &msg);

        println!("{} peer in the room", room.peers.len());
    }
}

async fn handle_tcp_stream(
    state: Arc<AppState>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    let lines = Framed::new(stream, LinesCodec::new());

    let (mut tx, mut rx) = lines.split();

    {
        let mut rooms = state.rooms.lock().unwrap();
        let room = rooms
            .entry("chatroom".to_string())
            .or_insert_with(Room::new);
    }

    let cloned_state = Arc::clone(&state);
    // Register our peer with state which internally sets up some channels.
    let peer = Peer::new(addr, "chatroom", state.clone())?;

    // A client has connected, let's let everyone know.
    {
        let mut rooms = state.rooms.lock().unwrap();
        let msg = format!("{} has joined the chat", addr);
        tracing::info!("{}", msg);
        let room = rooms.get_mut("chatroom").unwrap();
        room.broadcast(addr, &msg);
    }

    let mut send_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = rx.next().await {
            {
                println!(">>> {0} sent str: {msg:?}", addr);

                let rooms = cloned_state.rooms.lock();
                let mut rooms = rooms.unwrap();
                let room = rooms.get_mut("chatroom").unwrap();
                room.broadcast(addr, &msg);
            }
        }
    });
    let mut recv_task = tokio::spawn(async move {
        while let Ok(msg) = peer.rx.recv() {
            println!("Got a message: {}", msg);
            if tx.send(msg).await.is_err() {
                println!("Sending message failed");
                break;
            }
        }
    });
    // Process incoming messages until our stream is exhausted by a disconnect.

    tokio::select! {
        _ = (&mut send_task) => {
            recv_task.abort();
        },
        _ = (&mut recv_task) => {
            send_task.abort();
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        let mut rooms = state.rooms.lock().unwrap();

        let room = rooms.get_mut("chatroom").unwrap();
        room.peers.remove(&addr);
        let msg = format!("{} has left the chat", addr);
        room.broadcast(addr, &msg);
        println!("{} peer in the room", room.peers.len());
        tracing::info!("{}", msg);
    }

    Ok(())
}
