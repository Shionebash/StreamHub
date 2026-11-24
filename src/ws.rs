//! Conexiones WebSocket ligeras sobre el TLS del sistema.
//!
//! No se usa ningún motor web ni runtime asíncrono: solo el handshake
//! WebSocket (RFC 6455) sobre `native-tls`, que en Windows delega el TLS en
//! Schannel. Es el mismo motor TLS que ya usa `reqwest`, así que no se añade
//! ninguna pila criptográfica nueva al ejecutable.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::WebSocket;

/// Conexión WebSocket sobre TLS del sistema.
pub type Socket = WebSocket<native_tls::TlsStream<TcpStream>>;

/// Abre un WebSocket cifrado contra `host:443`.
///
/// El tiempo de espera de lectura permite atender comandos y detectar
/// desconexiones sin bloquear el hilo.
pub fn connect(host: &str, path: &str, timeout: Duration) -> Result<Socket, String> {
    let url = format!("wss://{host}{path}");
    let mut addrs: Vec<_> = (host, 443u16)
        .to_socket_addrs()
        .map_err(|err| format!("no se pudo resolver {host}: {err}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("sin direcciones para {host}"));
    }
    // Prefer IPv4 first so broken IPv6 routes do not stall or fail connections.
    addrs.sort_by_key(|addr| addr.is_ipv6());

    let mut last_err = None;
    let mut tcp_opt = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                tcp_opt = Some(stream);
                break;
            }
            Err(err) => last_err = Some(err),
        }
    }
    let tcp = tcp_opt.ok_or_else(|| {
        format!(
            "no se pudo conectar a {host}: {}",
            last_err
                .map(|err| err.to_string())
                .unwrap_or_else(|| "sin respuesta".to_string())
        )
    })?;
    let _ = tcp.set_nodelay(true);
    // Keep the full handshake timeout for TLS and the WebSocket HTTP upgrade.
    let _ = tcp.set_read_timeout(Some(timeout));
    let _ = tcp.set_write_timeout(Some(timeout));

    let connector = native_tls::TlsConnector::new().map_err(|err| err.to_string())?;
    let tls = connector
        .connect(host, tcp)
        .map_err(|err| format!("fallo TLS con {host}: {err}"))?;

    let request = url
        .as_str()
        .into_client_request()
        .map_err(|err| format!("URL inválida: {err}"))?;
    let (socket, _response) = tungstenite::client::client(request, tls)
        .map_err(|err| format!("fallo WebSocket con {host}: {err}"))?;
    // Switch to a short read timeout only after the handshake succeeds so
    // polling loops can check stop flags and periodic pings without blocking.
    let _ = socket
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(200)));
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connects_to_twitch_irc() {
        let token = crate::config::load_token().unwrap_or_default();
        let result = connect(CHAT_HOST_TEST, CHAT_PATH_TEST, Duration::from_secs(10));
        match result {
            Ok(mut socket) => {
                println!("conectado, token={} chars", token.len());
                let _ = socket.send(tungstenite::Message::Text(
                    "CAP REQ :twitch.tv/tags twitch.tv/commands".into(),
                ));
                let _ = socket.send(tungstenite::Message::Text(
                    format!("PASS oauth:{}", token.trim()).into(),
                ));
                let _ = socket.send(tungstenite::Message::Text("NICK probe".into()));
                let mut welcomed = false;
                for _ in 0..10 {
                    match socket.read() {
                        Ok(tungstenite::Message::Text(text)) => {
                            println!("< {text}");
                            if text.contains("001") {
                                welcomed = true;
                                break;
                            }
                        }
                        Ok(other) => println!("? {other:?}"),
                        Err(err) => {
                            println!("error de lectura: {err}");
                            break;
                        }
                    }
                }
                assert!(welcomed, "el servidor no devolvió el saludo");
            }
            Err(err) => panic!("fallo de conexión: {err}"),
        }
    }

    #[test]
    fn connects_to_twitch_pubsub() {
        let mut socket = connect("pubsub-edge.twitch.tv", "/v1", Duration::from_secs(10))
            .expect("should connect to Twitch PubSub");
        socket
            .send(tungstenite::Message::Text("{\"type\":\"PING\"}".into()))
            .expect("should send PING");
        let mut got_pong = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) if text.contains("PONG") => {
                    got_pong = true;
                    break;
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(err))
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut => {}
                Err(err) => panic!("read error: {err}"),
            }
        }
        let _ = socket.close(None);
        assert!(got_pong, "PubSub did not reply with PONG");
    }

    const CHAT_HOST_TEST: &str = "irc-ws.chat.twitch.tv";
    const CHAT_PATH_TEST: &str = "/websocket";
}
