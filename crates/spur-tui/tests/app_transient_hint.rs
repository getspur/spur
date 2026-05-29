use std::time::Duration;

use ratatui::{backend::TestBackend, Terminal};
use serde_json::Value;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_tui::action::Action;
use spur_tui::app::App;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::time::{sleep, timeout};

struct SocketCleanup {
    socket_path: std::path::PathBuf,
    _tempdir: tempfile::TempDir,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn bind_notebook_listener(label: &str) -> (String, SocketCleanup, UnixListener) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_nanos();
    let tempdir = tempfile::Builder::new()
        .prefix("nb")
        .tempdir_in("/tmp")
        .expect("notebook socket tempdir");
    let nonce = tempdir
        .path()
        .join(format!("n-{label}-{}-{nanos}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let socket_path = spur_core::notebook::control_socket_path(&nonce);
    let listener = UnixListener::bind(&socket_path).expect("bind notebook control socket");
    (
        nonce,
        SocketCleanup {
            socket_path,
            _tempdir: tempdir,
        },
        listener,
    )
}

async fn read_notebook_frame(stream: &mut tokio::net::UnixStream) -> Value {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await.expect("frame length");
    let len = u32::from_be_bytes(len) as usize;
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await.expect("frame body");
    serde_json::from_slice(&bytes).expect("request json")
}

async fn write_notebook_frame(stream: &mut tokio::net::UnixStream, value: Value) {
    let bytes = serde_json::to_vec(&value).expect("response json");
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .expect("write frame length");
    stream.write_all(&bytes).await.expect("write frame body");
    stream.flush().await.expect("flush frame");
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut rendered = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            rendered.push_str(buf[(x, y)].symbol());
        }
        rendered.push('\n');
    }
    rendered
}

#[test]
fn transient_hint_is_none_initially() {
    let app = App::new(None, false);

    assert!(app.transient_hint_for_test().is_none());
}

#[test]
fn flash_hint_short_sets_hint() {
    let mut app = App::new(None, false);

    app.flash_hint_short_for_test("hello");

    assert_eq!(
        app.transient_hint_for_test().map(|h| h.text.as_str()),
        Some("hello")
    );
}

#[tokio::test]
async fn successful_notebook_command_sets_hint_with_path() {
    let (nonce, _cleanup, listener) = bind_notebook_listener("success-hint");
    let expected_path = "/tmp/spur/notebook.md";
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept command");
        let request = read_notebook_frame(&mut stream).await;
        write_notebook_frame(
            &mut stream,
            serde_json::json!({
                "ok": true,
                "path": expected_path,
                "error": null
            }),
        )
        .await;
        request
    });

    let mut app = App::new(None, false);
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("notebook-a".into()),
        }),
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::NotebookSocketReady {
            session: SessionId("notebook-a".into()),
            socket_nonce: nonce,
        }),
    );

    spur_tui::test_support::process_action(&mut app, Action::NotebookCommand { arg: "new".into() });

    let request = timeout(Duration::from_millis(500), server)
        .await
        .expect("notebook command should reach shared socket")
        .expect("server task");
    assert_eq!(request["command"], "new");

    for _ in 0..20 {
        app.tick();
        if app
            .transient_hint_for_test()
            .is_some_and(|hint| hint.text.contains(expected_path))
        {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }

    let hint = app
        .transient_hint_for_test()
        .expect("successful command should set a transient hint");
    assert!(
        hint.text.contains(expected_path),
        "success hint should include path, got {:?}",
        hint.text
    );
}

#[test]
fn transient_hint_dismissed_after_tick_past_expiry() {
    let mut app = App::new(None, false);

    app.flash_hint_for_test("bye", Duration::ZERO);
    app.tick_transient_hint_for_test(std::time::Instant::now() + Duration::from_secs(10));

    assert!(app.transient_hint_for_test().is_none());
}

#[test]
fn transient_hint_overrides_status_bar_hint() {
    let mut app = App::new(None, false);
    app.flash_hint_for_test("temporary hint", Duration::from_secs(2));

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();

    let rendered = rendered_text(&terminal);
    assert!(
        rendered.contains("temporary hint"),
        "status bar should render transient hint:\n{rendered}"
    );
}
