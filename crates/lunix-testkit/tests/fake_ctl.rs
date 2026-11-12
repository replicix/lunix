//! `FakeCtlServer` speaks the real framing and messages.

use std::os::unix::net::UnixStream;

use lunix_iface::ctl::{
    encode_frame, from_payload, read_frame, to_payload, write_frame, CtlError, ErrorCode,
    NameParam, Outcome, Request, RequestFrame, Response, ResponseFrame, MAX_FRAME,
};
use lunix_testkit::ctl::method_name;
use lunix_testkit::FakeCtlServer;

fn call(conn: &mut UnixStream, id: u64, req: Request) -> ResponseFrame {
    let payload = to_payload(&RequestFrame::new(id, req)).unwrap();
    write_frame(conn, &payload, MAX_FRAME).unwrap();
    let back = read_frame(conn, MAX_FRAME).unwrap();
    from_payload(&back).unwrap()
}

fn name(n: &str) -> NameParam {
    NameParam { name: n.into() }
}

#[test]
fn ping_is_answered_and_scripts_apply_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let server = FakeCtlServer::start(&path).unwrap();
    let mut conn = UnixStream::connect(&path).unwrap();

    let r = call(&mut conn, 1, Request::Ping);
    assert_eq!(r.id, 1);
    assert!(matches!(r.result, Outcome::Ok(Response::Pong { .. })));

    server.respond("Detach", Outcome::Ok(Response::Detached));
    server.respond_once(
        "Detach",
        Outcome::Err(CtlError::new(ErrorCode::Busy, "in use")),
    );
    let first = call(&mut conn, 2, Request::Detach(name("v1")));
    assert!(matches!(first.result, Outcome::Err(ref e) if e.code == ErrorCode::Busy));
    let second = call(&mut conn, 3, Request::Detach(name("v1")));
    assert_eq!(second.id, 3);
    assert!(matches!(second.result, Outcome::Ok(Response::Detached)));

    let unscripted = call(&mut conn, 4, Request::VolumeList);
    assert!(matches!(unscripted.result, Outcome::Err(ref e) if e.code == ErrorCode::Internal));

    let seen: Vec<_> = server.requests().iter().map(method_name).collect();
    assert_eq!(seen, ["Ping", "Detach", "Detach", "VolumeList"]);
}

#[test]
fn invalid_requests_and_bad_frames_get_invalid_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let server = FakeCtlServer::start(&path).unwrap();
    let mut conn = UnixStream::connect(&path).unwrap();

    // A name that fails `Request::validate`.
    let r = call(&mut conn, 5, Request::Detach(name("bad name/with slash")));
    assert!(matches!(r.result, Outcome::Err(ref e) if e.code == ErrorCode::Invalid));
    // Wrong protocol version.
    let mut frame = RequestFrame::new(6, Request::Ping);
    frame.v = 99;
    write_frame(&mut conn, &to_payload(&frame).unwrap(), MAX_FRAME).unwrap();
    let back: ResponseFrame = from_payload(&read_frame(&mut conn, MAX_FRAME).unwrap()).unwrap();
    assert_eq!(back.id, 6);
    assert!(matches!(back.result, Outcome::Err(ref e) if e.code == ErrorCode::Invalid));
    // Garbage payload: id 0.
    write_frame(&mut conn, b"{not json", MAX_FRAME).unwrap();
    let back: ResponseFrame = from_payload(&read_frame(&mut conn, MAX_FRAME).unwrap()).unwrap();
    assert_eq!(back.id, 0);
    assert!(
        server.requests().is_empty(),
        "invalid requests are not recorded"
    );

    // An oversized frame header closes the connection after an error.
    use std::io::Write;
    let mut conn2 = UnixStream::connect(&path).unwrap();
    conn2.write_all(&(MAX_FRAME + 1).to_be_bytes()).unwrap();
    let back: ResponseFrame = from_payload(&read_frame(&mut conn2, MAX_FRAME).unwrap()).unwrap();
    assert!(matches!(back.result, Outcome::Err(ref e) if e.code == ErrorCode::Invalid));
    assert!(
        read_frame(&mut conn2, MAX_FRAME).is_err(),
        "connection closed"
    );
    let _ = encode_frame(b"x", MAX_FRAME).unwrap();
}

#[test]
fn dropping_the_server_closes_connections_and_removes_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let server = FakeCtlServer::start(&path).unwrap();
    let mut conn = UnixStream::connect(&path).unwrap();
    assert!(matches!(
        call(&mut conn, 1, Request::Ping).result,
        Outcome::Ok(_)
    ));
    drop(server);
    assert!(!path.exists());
    assert!(read_frame(&mut conn, MAX_FRAME).is_err());
}
